//! Background worker: owns the DB connection + CivitAI client off the UI thread.
//! UI sends `Cmd`, worker streams back `Event`. One thread, serial execution.

use crate::civitai::Client;
use crate::config::Config;
use crate::{db, pngmeta};
use eframe::egui;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};

pub enum Cmd {
    Reconfigure(Config),
    Sync,
    QueryPicks(db::PickFilter),
    QueryManifest,
    Download { file_id: i64, promote: bool, images: u32 },
    Promote(i64),
    Evict(i64),
    Lock(i64, bool),
    Audit,
    Heal(db::AuditReport),
}

pub enum Event {
    Status(String),
    Busy(bool),
    /// Sync progress: (combos_done, combos_total, rows_kept, unique_models).
    Progress(usize, usize, usize, usize),
    Log(String),
    Picks(Vec<db::PickRow>),
    Manifest(Vec<db::ManifestRow>),
    Audit(db::AuditReport),
    Error(String),
}

pub struct Worker {
    pub tx: Sender<Cmd>,
    pub rx: Receiver<Event>,
}

impl Worker {
    pub fn spawn(cfg: Config, ctx: egui::Context) -> Self {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<Cmd>();
        let (evt_tx, evt_rx) = std::sync::mpsc::channel::<Event>();
        std::thread::spawn(move || run(cfg, ctx, cmd_rx, evt_tx));
        Worker { tx: cmd_tx, rx: evt_rx }
    }
}

struct State {
    cfg: Config,
    conn: Option<rusqlite::Connection>,
    client: Client,
    ctx: egui::Context,
    tx: Sender<Event>,
}

impl State {
    fn emit(&self, e: Event) {
        let _ = self.tx.send(e);
        self.ctx.request_repaint();
    }
    fn status(&self, s: impl Into<String>) {
        self.emit(Event::Status(s.into()));
    }
}

fn run(cfg: Config, ctx: egui::Context, cmd_rx: Receiver<Cmd>, tx: Sender<Event>) {
    let client = Client::new(cfg.effective_token());
    let mut st = State {
        conn: db::open(&cfg.catalog_dir).ok(),
        client,
        cfg,
        ctx,
        tx,
    };
    if st.conn.is_none() {
        st.emit(Event::Error("could not open catalog.sqlite".into()));
    }
    while let Ok(cmd) = cmd_rx.recv() {
        handle(&mut st, cmd);
    }
}

fn handle(st: &mut State, cmd: Cmd) {
    match cmd {
        Cmd::Reconfigure(cfg) => {
            st.conn = db::open(&cfg.catalog_dir).ok();
            st.client = Client::new(cfg.effective_token());
            st.cfg = cfg;
            st.status("reconfigured");
        }
        Cmd::QueryPicks(f) => {
            if let Some(conn) = &st.conn {
                match db::query_picks(conn, &f) {
                    Ok(rows) => st.emit(Event::Picks(rows)),
                    Err(e) => st.emit(Event::Error(e)),
                }
            }
        }
        Cmd::QueryManifest => {
            if let Some(conn) = &st.conn {
                match db::query_manifest(conn) {
                    Ok(rows) => st.emit(Event::Manifest(rows)),
                    Err(e) => st.emit(Event::Error(e)),
                }
            }
        }
        Cmd::Sync => sync(st),
        Cmd::Download { file_id, promote, images } => {
            download(st, file_id, promote, images)
        }
        Cmd::Promote(id) => promote(st, id),
        Cmd::Evict(id) => evict(st, id),
        Cmd::Lock(id, v) => {
            if let Some(conn) = &st.conn {
                db::set_locked(conn, id, v);
                db::log(conn, id, 0, if v { "lock" } else { "unlock" }, "");
            }
            st.status(if v { "locked" } else { "unlocked" });
            refresh_manifest(st);
        }
        Cmd::Audit => audit(st),
        Cmd::Heal(rep) => {
            if let Some(conn) = &st.conn {
                let n = db::heal(conn, &rep);
                st.status(format!("healed {n} manifest rows"));
            }
            refresh_manifest(st);
        }
    }
}

fn refresh_manifest(st: &State) {
    if let Some(conn) = &st.conn {
        if let Ok(rows) = db::query_manifest(conn) {
            st.emit(Event::Manifest(rows));
        }
    }
}

const QUERIES: [(&str, &str); 3] = [
    ("Most Downloaded", "AllTime"),
    ("Highest Rated", "AllTime"),
    ("Newest", "Month"),
];

fn sync(st: &mut State) {
    if st.conn.is_none() {
        st.emit(Event::Error("no catalog db".into()));
        return;
    }
    st.emit(Event::Busy(true));
    let cfg = st.cfg.clone();
    let mut seen: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let mut total = 0usize;
    let combos_total = cfg.types.len() * cfg.base_models.len() * QUERIES.len();
    let mut combo = 0usize;
    st.emit(Event::Progress(0, combos_total, 0, 0));
    for t in &cfg.types {
        for b in &cfg.base_models {
            for (sort, period) in QUERIES {
                combo += 1;
                st.status(format!(
                    "[{combo}/{combos_total}] {t} / {b} / {sort} {period}"
                ));
                let mut cursor: Option<String> = None;
                let mut kept = 0u32;
                loop {
                    let page = st.client.models_page(
                        t, b, sort, period, cfg.nsfw, 100, cursor.as_deref(),
                    );
                    let page = match page {
                        Ok(p) => p,
                        Err(e) => {
                            st.emit(Event::Log(format!("✘ {t}/{b}/{sort}: {e}")));
                            break;
                        }
                    };
                    if page.items.is_empty() {
                        break;
                    }
                    // one transaction per page → one fsync instead of ~300
                    if let Some(conn) = &st.conn {
                        let _ = conn.execute_batch("BEGIN");
                        for m in &page.items {
                            let _ = db::upsert_model(conn, m);
                            let mid = m.get("id").and_then(|x| x.as_i64()).unwrap_or(0);
                            if mid != 0 {
                                seen.insert(mid);
                            }
                            kept += 1;
                            total += 1;
                            if kept >= cfg.top_n {
                                break;
                            }
                        }
                        let _ = conn.execute_batch("COMMIT");
                    }
                    st.emit(Event::Progress(combo - 1, combos_total, total, seen.len()));
                    if kept >= cfg.top_n {
                        break;
                    }
                    match page.next_cursor {
                        Some(c) => cursor = Some(c),
                        None => break,
                    }
                }
                st.emit(Event::Log(format!(
                    "✔ [{combo}/{combos_total}] {t} · {b} · {sort} → {kept} models"
                )));
                st.emit(Event::Progress(combo, combos_total, total, seen.len()));
            }
        }
    }
    st.emit(Event::Busy(false));
    st.emit(Event::Log(format!(
        "── sync complete: {} rows, {} unique models ──",
        total,
        seen.len()
    )));
    st.status(format!("sync complete — {} unique models", seen.len()));
}

fn vault_dest(cfg: &Config, model_type: &str, file_name: &str) -> PathBuf {
    Path::new(&cfg.vault_root)
        .join(Config::subdir_for(model_type))
        .join(file_name)
}

fn download(st: &mut State, file_id: i64, do_promote: bool, images: u32) {
    let Some(conn) = &st.conn else { return };
    // pull download url + identity for this file
    let row = conn.query_row(
        "SELECT f.download_url, f.name, m.type, f.sha256, v.model_id
         FROM files f JOIN versions v ON v.version_id=f.version_id
         JOIN models m ON m.model_id=v.model_id WHERE f.file_id=?1",
        [file_id],
        |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, i64>(4)?,
            ))
        },
    );
    let (url, name, mtype, sha_expected, model_id) = match row {
        Ok((Some(u), Some(n), mt, sha, mid)) => {
            (u, n, mt.unwrap_or_default(), sha, mid)
        }
        _ => {
            st.emit(Event::Error(format!("file {file_id} not downloadable")));
            return;
        }
    };
    let dest = vault_dest(&st.cfg, &mtype, &name);
    if dest.exists() {
        if let Some(conn) = &st.conn {
            db::set_downloaded(conn, file_id, &dest.to_string_lossy(),
                sha_expected.as_deref().unwrap_or(""));
        }
        st.status(format!("{name} already in vault"));
    } else {
        st.emit(Event::Busy(true));
        st.status(format!("downloading {name}"));
        let ctx = st.ctx.clone();
        let tx = st.tx.clone();
        let nm = name.clone();
        let res = st.client.download_file(&url, &dest, |done, total| {
            let pct = if total > 0 { done * 100 / total } else { 0 };
            if done == total || done % (16 << 20) < (1 << 20) {
                let _ = tx.send(Event::Status(format!(
                    "downloading {nm}  {pct}%  {}/{} MB",
                    done >> 20, total >> 20
                )));
                ctx.request_repaint();
            }
        });
        st.emit(Event::Busy(false));
        match res {
            Ok(sha) => {
                if let Some(exp) = &sha_expected {
                    if !exp.is_empty() && !exp.eq_ignore_ascii_case(&sha) {
                        let _ = std::fs::remove_file(&dest);
                        st.emit(Event::Error(format!("SHA256 mismatch for {name} — discarded")));
                        return;
                    }
                }
                if let Some(conn) = &st.conn {
                    db::set_downloaded(conn, file_id, &dest.to_string_lossy(), &sha);
                    db::log(conn, file_id, model_id, "download", &dest.to_string_lossy());
                }
                st.status(format!("verified {name}"));
            }
            Err(e) => {
                st.emit(Event::Error(format!("download failed: {e}")));
                return;
            }
        }
    }
    // full example-image + workflow harvest for this model
    if images > 0 {
        st.status(format!("harvesting {images} images for model {model_id}"));
        harvest_images(st, model_id, images, false);
    }
    if do_promote {
        promote(st, file_id);
    } else {
        refresh_manifest(st);
    }
}

fn promote(st: &mut State, file_id: i64) {
    let Some(conn) = &st.conn else { return };
    let Some(row) = db::file_row(conn, file_id) else { return };
    let Some(src) = row.local_path.clone() else {
        st.emit(Event::Error("not in vault — download first".into()));
        return;
    };
    if !Path::new(&src).exists() {
        st.emit(Event::Error("vault file missing — run audit".into()));
        return;
    }
    let dst = Path::new(&st.cfg.nvme_root)
        .join(Config::subdir_for(&row.model_type))
        .join(&row.file_name);
    if let Some(p) = dst.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    st.emit(Event::Busy(true));
    st.status(format!("hotloading {} to NVMe", row.file_name));
    let res = std::fs::copy(&src, &dst);
    st.emit(Event::Busy(false));
    match res {
        Ok(_) => {
            db::set_promoted(conn, file_id, &dst.to_string_lossy());
            db::log(conn, file_id, row.model_id, "promote", &dst.to_string_lossy());
            st.status(format!("active on NVMe: {}", row.file_name));
        }
        Err(e) => st.emit(Event::Error(format!("hotload failed: {e}"))),
    }
    refresh_manifest(st);
}

fn evict(st: &mut State, file_id: i64) {
    let Some(conn) = &st.conn else { return };
    let Some(row) = db::file_row(conn, file_id) else { return };
    if row.locked {
        st.emit(Event::Error("replica is locked — unlock before evicting".into()));
        return;
    }
    if let Some(np) = &row.nvme_path {
        let _ = std::fs::remove_file(np);
    }
    db::set_evicted(conn, file_id);
    db::log(conn, file_id, row.model_id, "evict", "");
    st.status(format!("evicted {} from NVMe", row.file_name));
    refresh_manifest(st);
}

fn audit(st: &mut State) {
    let Some(conn) = &st.conn else { return };
    st.emit(Event::Busy(true));
    st.status("auditing registry…");
    match db::audit(conn, &st.cfg.vault_root) {
        Ok(rep) => {
            let msg = format!(
                "audit: {} checked, {} missing-vault, {} missing-nvme, {} orphans",
                rep.checked,
                rep.missing_vault.len(),
                rep.missing_nvme.len(),
                rep.orphans.len()
            );
            st.emit(Event::Audit(rep));
            st.status(msg);
        }
        Err(e) => st.emit(Event::Error(e)),
    }
    st.emit(Event::Busy(false));
}

// ---- example image harvest (shared by sync starter + download full) --------

fn ext_for(ct: &str) -> &'static str {
    match ct {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "video/mp4" => "mp4",
        _ => "bin",
    }
}

fn harvest_images(st: &mut State, model_id: i64, per: u32, starter: bool) {
    let Some(raw) = st.conn.as_ref().and_then(|c| db::model_raw(c, model_id)) else {
        return;
    };
    let imgs: Vec<Value> = raw
        .get("modelVersions")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|v0| v0.get("images"))
        .and_then(|i| i.as_array())
        .cloned()
        .unwrap_or_default();

    let take: Vec<&Value> = if starter {
        imgs.iter()
            .find(|i| i.get("type").and_then(|t| t.as_str()) == Some("image"))
            .or_else(|| imgs.first())
            .into_iter()
            .collect()
    } else {
        imgs.iter().take(per as usize).collect()
    };

    let gallery_root = PathBuf::from(&st.cfg.gallery_root);
    let include_video = st.cfg.include_video;
    for im in take {
        let img_id = im.get("id").and_then(|x| x.as_i64()).unwrap_or(0);
        if img_id == 0 {
            continue;
        }
        let mtype = im.get("type").and_then(|t| t.as_str()).unwrap_or("image");
        if mtype == "video" && !include_video {
            continue;
        }
        if let Some(conn) = &st.conn {
            if let Some(p) = db::image_exists(conn, img_id) {
                if Path::new(&p).exists() {
                    continue;
                }
            }
        }
        let url = match im.get("url").and_then(|u| u.as_str()) {
            Some(u) => u.to_string(),
            None => continue,
        };
        let (ct, bytes) = match st.client.get_bytes(&url) {
            Ok(x) => x,
            Err(_) => continue,
        };
        let ext = ext_for(&ct);
        let mdir = gallery_root.join(model_id.to_string());
        let _ = std::fs::create_dir_all(&mdir);
        let fpath = mdir.join(format!("{img_id}.{ext}"));
        if std::fs::write(&fpath, &bytes).is_err() {
            continue;
        }
        let mut wf_path: Option<String> = None;
        let mut params_path: Option<String> = None;
        let mut has_wf = false;
        if ext == "png" {
            let chunks = pngmeta::text_chunks(&bytes);
            let (wf, params) = pngmeta::split_meta(&chunks);
            if let Some(w) = wf {
                let p = mdir.join(format!("{img_id}.workflow.json"));
                let _ = std::fs::write(&p, w);
                wf_path = Some(p.to_string_lossy().into_owned());
                has_wf = true;
            }
            if let Some(pr) = params {
                let p = mdir.join(format!("{img_id}.params.txt"));
                let _ = std::fs::write(&p, pr);
                params_path = Some(p.to_string_lossy().into_owned());
            }
        }
        if let Some(conn) = &st.conn {
            db::record_image(
                conn, img_id, model_id, &url, mtype,
                im.get("nsfwLevel").and_then(|x| x.as_str()),
                im.get("width").and_then(|x| x.as_i64()),
                im.get("height").and_then(|x| x.as_i64()),
                &fpath.to_string_lossy(),
                wf_path.as_deref(), params_path.as_deref(), has_wf, starter,
            );
        }
    }
}
