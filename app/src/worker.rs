//! Background worker: owns the DB connection + CivitAI client off the UI thread.
//! UI sends `Cmd`, worker streams back `Event`. One thread, serial execution.

use crate::backends::{self, GenRequest};
use crate::civitai::Client;
use crate::config::{Config, Project};
use crate::{db, pngmeta, project};
use eframe::egui;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub enum Cmd {
    Reconfigure(Config),
    /// Open an IP as the active project (opens its per-IP project.sqlite).
    SetProject(Project),
    /// Forge: generate an image for the active IP (Phase 1).
    Generate {
        req: GenRequest,
        entity: String,
    },
    /// Refresh the Forge tab's job + asset lists.
    QueryForge,
    /// Asset manager: scan the IP asset vault and register new media files.
    ScanAssets,
    /// Asset manager: browse assets, filtered by kind bucket + entity substring.
    QueryAssets {
        kind: Option<String>,
        entity: Option<String>,
    },
    /// Asset manager: copy an asset into the IP's engine tree under a topic.
    PlaceAsset {
        id: i64,
        topic: String,
    },
    /// Prompt matrix: browse / save / delete / import-from-prompts.md.
    QueryPrompts {
        entity: Option<String>,
    },
    SavePrompt(project::PromptRow),
    DeletePrompt(i64),
    ImportPrompts {
        entity: String,
    },
    /// Lore: rebuild the lore index from the IP's lore repo.
    ReindexLore,
    /// Lore: browse the index, filtered by kind + free-text search.
    QueryLore {
        kind: Option<String>,
        search: Option<String>,
    },
    /// Lore: read one entry's full markdown for the reader panel.
    ReadLore(i64),
    /// Forge: burst — generate `count` image variations across seeds.
    RunBurst {
        req: GenRequest,
        entity: String,
        count: u32,
    },
    /// Composite pipeline: run a named build graph for an entity.
    RunPipeline {
        name: String,
        entity: String,
        req: GenRequest,
    },
    /// Refresh the Pipelines tab's run list.
    QueryPipelines,
    /// Release authority: cut a freeze / ship-cut for the active IP.
    CreateRelease {
        name: String,
        kind: String,
    },
    /// Refresh the Releases tab.
    QueryReleases,
    /// Export a stored release manifest to disk.
    ExportRelease(i64),
    Sync,
    QueryPicks(db::PickFilter),
    QueryManifest,
    Download {
        file_id: i64,
        promote: bool,
        images: u32,
    },
    Promote(i64),
    Evict(i64),
    Lock(i64, bool),
    Audit,
    Heal(db::AuditReport),
    /// Harvest example images + workflows for every tracked model.
    HarvestImages,
    /// Identify orphan files by hash via CivitAI, import + adopt what's found.
    RecoverOrphans(Vec<String>),
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
    /// A cover landed on disk: (model_id, local_path).
    CoverReady(i64, String),
    CoverFailed(i64),
    /// The active project was opened; carries its roots + dashboard counts.
    ProjectInfo(ProjectInfo),
    /// Forge tab data: recent jobs + assets for the active IP.
    ForgeState {
        jobs: Vec<project::JobRow>,
        assets: Vec<project::AssetRow>,
    },
    /// Asset manager browse results.
    Assets(Vec<project::AssetRow>),
    /// Prompt matrix rows.
    Prompts(Vec<project::PromptRow>),
    /// Lore index rows + the distinct kinds present (for filter chips).
    Lore {
        entries: Vec<crate::lore::LoreEntry>,
        kinds: Vec<String>,
    },
    /// One lore entry's full text: (entry, body).
    LoreText {
        entry: crate::lore::LoreEntry,
        body: String,
    },
    /// Composite pipeline run list.
    Pipelines(Vec<project::PipelineRun>),
    /// Release list for the active IP.
    Releases(Vec<project::ReleaseRow>),
    Error(String),
}

/// Snapshot of the open IP for the dashboard.
#[derive(Clone)]
pub struct ProjectInfo {
    pub name: String,
    pub lore_root: String,
    pub engine_root: String,
    pub db_path: String,
    pub asset_vault: String,
    pub lore_root_exists: bool,
    pub stats: project::ProjectStats,
}

pub struct Worker {
    pub tx: Sender<Cmd>,
    pub rx: Receiver<Event>,
    pub evt_tx: Sender<Event>, // shared so side pools (covers) can report back
}

impl Worker {
    pub fn spawn(cfg: Config, ctx: egui::Context) -> Self {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<Cmd>();
        let (evt_tx, evt_rx) = std::sync::mpsc::channel::<Event>();
        let evt_for_thread = evt_tx.clone();
        std::thread::spawn(move || run(cfg, ctx, cmd_rx, evt_for_thread));
        Worker {
            tx: cmd_tx,
            rx: evt_rx,
            evt_tx,
        }
    }
}

/// One cover to fetch into the on-disk cache.
pub struct CoverReq {
    pub model_id: i64,
    pub url: String,
}

/// A small pool of threads that fill the cover cache in parallel — independent
/// of the main worker, so covers load fast and never queue behind big downloads.
pub struct CoverFetcher {
    pub tx: Sender<CoverReq>,
}

impl CoverFetcher {
    pub fn spawn(cfg: &Config, ctx: egui::Context, evt_tx: Sender<Event>, threads: usize) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<CoverReq>();
        let rx = Arc::new(Mutex::new(rx));
        let dir = cfg.covers_dir();
        let token = cfg.effective_token();
        let client = reqwest::blocking::Client::builder()
            .user_agent("synthetrix-harvester/1.0")
            .timeout(Duration::from_secs(60))
            .build()
            .expect("cover client");
        for _ in 0..threads.max(1) {
            let rx = rx.clone();
            let evt = evt_tx.clone();
            let ctx = ctx.clone();
            let dir = dir.clone();
            let client = client.clone();
            let token = token.clone();
            std::thread::spawn(move || loop {
                let req = {
                    let guard = match rx.lock() {
                        Ok(g) => g,
                        Err(_) => break,
                    };
                    guard.recv()
                };
                let req = match req {
                    Ok(r) => r,
                    Err(_) => break, // channel closed
                };
                let mid = req.model_id;
                // disk cache hit?
                let mut path = None;
                for ext in ["jpg", "png", "webp"] {
                    let p = dir.join(format!("{mid}.{ext}"));
                    if p.exists() {
                        path = Some(p);
                        break;
                    }
                }
                if path.is_none() {
                    let url = req.url.replace("width=256", "width=384");
                    let mut rb = client.get(&url);
                    if let Some(t) = &token {
                        rb = rb.bearer_auth(t);
                    }
                    if let Ok(resp) = rb.send().and_then(|r| r.error_for_status()) {
                        let ct = resp
                            .headers()
                            .get(reqwest::header::CONTENT_TYPE)
                            .and_then(|h| h.to_str().ok())
                            .unwrap_or("")
                            .split(';')
                            .next()
                            .unwrap_or("")
                            .to_string();
                        if let Ok(bytes) = resp.bytes() {
                            let _ = std::fs::create_dir_all(&dir);
                            let p = dir.join(format!("{mid}.{}", ext_for(&ct)));
                            if std::fs::write(&p, &bytes).is_ok() {
                                path = Some(p);
                            }
                        }
                    }
                }
                match path {
                    Some(p) => {
                        let _ = evt.send(Event::CoverReady(mid, p.to_string_lossy().into_owned()));
                    }
                    None => {
                        let _ = evt.send(Event::CoverFailed(mid));
                    }
                }
                ctx.request_repaint();
            });
        }
        CoverFetcher { tx }
    }
}

struct State {
    cfg: Config,
    conn: Option<rusqlite::Connection>,
    project_conn: Option<rusqlite::Connection>,
    active_project: Option<Project>,
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
        project_conn: None,
        active_project: None,
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
        Cmd::SetProject(p) => set_project(st, p),
        Cmd::Generate { req, entity } => generate(st, req, entity),
        Cmd::QueryForge => refresh_forge(st),
        Cmd::ScanAssets => scan_assets(st),
        Cmd::QueryAssets { kind, entity } => query_assets_cmd(st, kind, entity),
        Cmd::PlaceAsset { id, topic } => place_asset(st, id, topic),
        Cmd::QueryPrompts { entity } => query_prompts_cmd(st, entity),
        Cmd::SavePrompt(p) => {
            if let Some(c) = &st.project_conn {
                project::upsert_prompt(c, &p);
            }
            query_prompts_cmd(st, None);
            emit_project_info(st);
        }
        Cmd::DeletePrompt(id) => {
            if let Some(c) = &st.project_conn {
                project::delete_prompt(c, id);
            }
            query_prompts_cmd(st, None);
            emit_project_info(st);
        }
        Cmd::ImportPrompts { entity } => import_prompts(st, entity),
        Cmd::ReindexLore => reindex_lore(st),
        Cmd::QueryLore { kind, search } => query_lore_cmd(st, kind, search),
        Cmd::ReadLore(id) => read_lore(st, id),
        Cmd::RunBurst { req, entity, count } => run_burst(st, req, entity, count),
        Cmd::RunPipeline { name, entity, req } => run_pipeline(st, name, entity, req),
        Cmd::QueryPipelines => refresh_pipelines(st),
        Cmd::CreateRelease { name, kind } => create_release(st, name, kind),
        Cmd::QueryReleases => refresh_releases(st),
        Cmd::ExportRelease(id) => export_release(st, id),
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
        Cmd::Download {
            file_id,
            promote,
            images,
        } => download(st, file_id, promote, images),
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
        Cmd::HarvestImages => harvest_all(st),
        Cmd::RecoverOrphans(paths) => recover_orphans(st, paths),
        Cmd::Audit => audit(st),
        Cmd::Heal(rep) => {
            if let Some(conn) = &st.conn {
                let h = db::heal(conn, &rep);
                st.status(format!(
                    "heal: adopted {} orphan(s), reset {}, {} unmatched (not in catalog)",
                    h.adopted, h.reset, h.unmatched
                ));
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

/// Open an IP as the active project: open (or create) its project.sqlite and
/// report its roots + counts to the dashboard.
fn set_project(st: &mut State, p: Project) {
    match project::open(&p.project_db_path(), &p.name) {
        Ok(conn) => {
            st.project_conn = Some(conn);
            st.active_project = Some(p.clone());
            emit_project_info(st);
            refresh_forge(st);
            query_assets_cmd(st, None, None);
            query_prompts_cmd(st, None);
            // First open of an IP builds the lore index; later opens reuse it
            // (an explicit Reindex button refreshes on demand).
            if let Some(c) = &st.project_conn {
                if crate::lore::count(c) == 0 && Path::new(&p.lore_root).is_dir() {
                    let n = crate::lore::reindex(c, Path::new(&p.lore_root));
                    st.emit(Event::Log(format!("lore: indexed {n} docs for {}", p.name)));
                }
            }
            query_lore_cmd(st, None, None);
            refresh_pipelines(st);
            refresh_releases(st);
            st.status(format!("project: {}", p.name));
        }
        Err(e) => {
            st.project_conn = None;
            st.active_project = None;
            st.emit(Event::Error(format!("open project {}: {e}", p.name)));
        }
    }
}

/// (Re)emit the active project's roots + live counts to the dashboard.
fn emit_project_info(st: &State) {
    if let (Some(p), Some(c)) = (&st.active_project, &st.project_conn) {
        st.emit(Event::ProjectInfo(ProjectInfo {
            name: p.name.clone(),
            lore_root: p.lore_root.clone(),
            engine_root: p.engine_root.clone(),
            db_path: p.project_db_path().to_string_lossy().into_owned(),
            asset_vault: p.asset_vault_path().to_string_lossy().into_owned(),
            lore_root_exists: Path::new(&p.lore_root).is_dir(),
            stats: project::stats(c),
        }));
    }
}

fn refresh_forge(st: &State) {
    if let Some(c) = &st.project_conn {
        st.emit(Event::ForgeState {
            jobs: project::recent_jobs(c, 50),
            assets: project::recent_assets(c, 50),
        });
    }
}

/// Phase-1 forge: text→image for the active IP via the local ComfyUI backend.
/// Saves to the IP asset vault, writes a provenance sidecar, registers the asset
/// + job in project.sqlite.
fn generate(st: &mut State, req: GenRequest, entity: String) {
    let Some(project) = st.active_project.clone() else {
        st.emit(Event::Error("no project open — pick an IP first".into()));
        return;
    };
    if st.project_conn.is_none() {
        st.emit(Event::Error("project DB not open".into()));
        return;
    }
    let params = serde_json::to_string(&serde_json::json!({
        "model": req.model, "negative": req.negative, "width": req.width,
        "height": req.height, "steps": req.steps, "cfg": req.cfg,
        "sampler": req.sampler, "scheduler": req.scheduler, "seed": req.seed
    }))
    .unwrap_or_default();
    let ent = if entity.trim().is_empty() {
        "untitled".to_string()
    } else {
        entity.trim().to_string()
    };
    let mut backend = backends::backend_for("comfy_local", &st.cfg.comfy_url);
    let job_id = match &st.project_conn {
        Some(c) => project::insert_job(c, "image", backend.id(), &ent, &req.prompt, &params),
        None => 0,
    };
    refresh_forge(st);
    st.emit(Event::Busy(true));
    let ctx = st.ctx.clone();
    let tx = st.tx.clone();
    let res = backend.generate_image(&req, &mut |frac, note| {
        let _ = tx.send(Event::Status(format!("forge: {note} {:.0}%", frac * 100.0)));
        ctx.request_repaint();
    });
    st.emit(Event::Busy(false));

    match res {
        Ok(gen) => {
            let ext = image_ext(&gen.content_type);
            let stem = format!("{ent}_{job_id}");
            match store_asset(
                st,
                &project,
                "image",
                "images",
                &stem,
                ext,
                &gen.bytes,
                &gen.content_type,
                &ent,
                &gen.meta,
                job_id,
            ) {
                Ok((_id, pstr)) => {
                    if let Some(c) = &st.project_conn {
                        project::update_job(
                            c,
                            job_id,
                            "done",
                            Some(&pstr),
                            &format!("seed {}", gen.seed),
                        );
                    }
                    st.status(format!("forge: saved {stem}.{ext} (seed {})", gen.seed));
                }
                Err(e) => {
                    if let Some(c) = &st.project_conn {
                        project::update_job(c, job_id, "failed", None, &format!("write: {e}"));
                    }
                    st.emit(Event::Error(format!("forge save: {e}")));
                }
            }
        }
        Err(e) => {
            if let Some(c) = &st.project_conn {
                project::update_job(c, job_id, "failed", None, &e);
            }
            st.emit(Event::Error(format!("forge: {e}")));
        }
    }
    refresh_forge(st);
    emit_project_info(st);
}

fn image_ext(content_type: &str) -> &'static str {
    match content_type {
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        _ => "png",
    }
}

/// Write produced media into the IP asset vault (`<vault>/<subdir>/<stem>.<ext>`)
/// with a JSON provenance sidecar, hash it, and register the asset. Returns
/// (asset_id, absolute_path).
#[allow(clippy::too_many_arguments)]
fn store_asset(
    st: &State,
    project: &Project,
    kind: &str,
    subdir: &str,
    stem: &str,
    ext: &str,
    bytes: &[u8],
    content_type: &str,
    entity: &str,
    meta: &serde_json::Value,
    job_id: i64,
) -> Result<(i64, String), String> {
    let dir = project.asset_vault_path().join(subdir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let name = format!("{stem}.{ext}");
    let path = dir.join(&name);
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    let _ = std::fs::write(
        dir.join(format!("{stem}.json")),
        serde_json::to_string_pretty(meta).unwrap_or_default(),
    );
    let sha = sha256_bytes(bytes);
    let pstr = path.to_string_lossy().into_owned();
    let id = match &st.project_conn {
        Some(c) => project::insert_asset(
            c,
            kind,
            &name,
            entity,
            content_type,
            &pstr,
            &sha,
            &meta.to_string(),
            job_id,
        ),
        None => 0,
    };
    Ok((id, pstr))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

// ---- Phase 2: Asset Manager ------------------------------------------------

fn query_assets_cmd(st: &State, kind: Option<String>, entity: Option<String>) {
    if let Some(c) = &st.project_conn {
        let rows = project::query_assets(
            c,
            kind.as_deref(),
            entity.as_deref().filter(|s| !s.is_empty()),
            500,
        );
        st.emit(Event::Assets(rows));
    }
}

/// Walk the IP asset vault and register any media files not yet tracked.
fn scan_assets(st: &mut State) {
    let Some(project) = st.active_project.clone() else {
        st.emit(Event::Error("no project open".into()));
        return;
    };
    if st.project_conn.is_none() {
        return;
    }
    st.emit(Event::Busy(true));
    let root = project.asset_vault_path();
    let mut stack = vec![root];
    let mut added = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let (kind, mt) = project::media_type_for(&p);
            if kind == "other" {
                continue; // skip sidecars / unknowns
            }
            let ps = p.to_string_lossy().into_owned();
            if let Some(c) = &st.project_conn {
                if project::asset_exists(c, &ps) {
                    continue;
                }
                let parent = p
                    .parent()
                    .and_then(|d| d.file_name())
                    .and_then(|n| n.to_str())
                    .unwrap_or("");
                let entity = if matches!(parent, "images" | "video" | "audio" | "meshes" | "assets")
                {
                    String::new()
                } else {
                    parent.to_string()
                };
                let name = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();
                project::insert_scanned_asset(c, kind, &name, &entity, &mt, &ps, "");
                added += 1;
            }
        }
    }
    st.emit(Event::Busy(false));
    st.status(format!("asset scan: +{added} new"));
    query_assets_cmd(st, None, None);
    emit_project_info(st);
}

/// Copy an asset into the IP's engine tree: <engine_root>/Content/Generated/<topic>/.
fn place_asset(st: &mut State, id: i64, topic: String) {
    let Some(project) = st.active_project.clone() else {
        st.emit(Event::Error("no project open".into()));
        return;
    };
    if project.engine_root.trim().is_empty() {
        st.emit(Event::Error(
            "this IP has no engine root set (Settings)".into(),
        ));
        return;
    }
    let Some(row) = st
        .project_conn
        .as_ref()
        .and_then(|c| project::asset_by_id(c, id))
    else {
        st.emit(Event::Error("asset not found".into()));
        return;
    };
    let src = Path::new(&row.path);
    if !src.is_file() {
        st.emit(Event::Error("asset file missing on disk".into()));
        return;
    }
    let dest_dir = Path::new(&project.engine_root)
        .join("Content")
        .join("Generated")
        .join(&topic);
    let _ = std::fs::create_dir_all(&dest_dir);
    let dest = dest_dir.join(&row.name);
    match std::fs::copy(src, &dest) {
        Ok(_) => {
            let dp = dest.to_string_lossy().into_owned();
            if let Some(c) = &st.project_conn {
                project::set_engine_path(c, id, &dp);
            }
            st.status(format!("placed {} → {topic}", row.name));
        }
        Err(e) => st.emit(Event::Error(format!("place: {e}"))),
    }
    query_assets_cmd(st, None, None);
}

// ---- Phase 3: Prompt Matrix ------------------------------------------------

fn query_prompts_cmd(st: &State, entity: Option<String>) {
    if let Some(c) = &st.project_conn {
        let rows = project::query_prompts(c, entity.as_deref().filter(|s| !s.is_empty()), 500);
        st.emit(Event::Prompts(rows));
    }
}

/// Import a per-entity `prompts.md` from the lore repo into the prompt matrix.
fn import_prompts(st: &mut State, entity: String) {
    let Some(project) = st.active_project.clone() else {
        st.emit(Event::Error("no project open".into()));
        return;
    };
    let ent = entity.trim().to_string();
    if ent.is_empty() {
        st.emit(Event::Error("enter an entity name to import".into()));
        return;
    }
    // find <lore_root>/**/<entity>/prompts.md
    let mut found = None;
    let mut stack = vec![std::path::PathBuf::from(&project.lore_root)];
    'walk: while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if !p.is_dir() {
                continue;
            }
            let dn = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if dn.starts_with('.') {
                continue;
            }
            if dn.eq_ignore_ascii_case(&ent) {
                let pm = p.join("prompts.md");
                if pm.is_file() {
                    found = Some(pm);
                    break 'walk;
                }
            }
            stack.push(p);
        }
    }
    let Some(pm) = found else {
        st.emit(Event::Error(format!(
            "no <{ent}>/prompts.md found under {}",
            project.lore_root
        )));
        return;
    };
    let text = match std::fs::read_to_string(&pm) {
        Ok(t) => t,
        Err(e) => {
            st.emit(Event::Error(format!("read prompts.md: {e}")));
            return;
        }
    };
    let rows = project::parse_prompts_md(&ent, &text);
    let n = rows.len();
    if let Some(c) = &st.project_conn {
        for r in &rows {
            project::upsert_prompt(c, r);
        }
    }
    st.status(format!("imported {n} prompts for {ent}"));
    query_prompts_cmd(st, None);
    emit_project_info(st);
}

// ---- Phase 4: Lore subsystem -----------------------------------------------

fn query_lore_cmd(st: &State, kind: Option<String>, search: Option<String>) {
    if let Some(c) = &st.project_conn {
        let entries = crate::lore::query(
            c,
            kind.as_deref().filter(|s| !s.is_empty()),
            search.as_deref().filter(|s| !s.is_empty()),
            2000,
        );
        let kinds = crate::lore::kinds(c);
        st.emit(Event::Lore { entries, kinds });
    }
}

/// Rebuild the lore index from the IP's lore repo, then re-emit the browse view.
fn reindex_lore(st: &mut State) {
    let Some(project) = st.active_project.clone() else {
        st.emit(Event::Error("no project open".into()));
        return;
    };
    let root = Path::new(&project.lore_root);
    if !root.is_dir() {
        st.emit(Event::Error(format!(
            "lore root missing: {}",
            project.lore_root
        )));
        return;
    }
    st.emit(Event::Busy(true));
    let n = match &st.project_conn {
        Some(c) => crate::lore::reindex(c, root),
        None => 0,
    };
    st.emit(Event::Busy(false));
    st.status(format!("lore: re-indexed {n} docs"));
    query_lore_cmd(st, None, None);
    emit_project_info(st);
}

/// Read one lore entry's full markdown for the reader panel.
fn read_lore(st: &State, id: i64) {
    let Some(project) = st.active_project.as_ref() else {
        return;
    };
    let Some(entry) = st
        .project_conn
        .as_ref()
        .and_then(|c| crate::lore::entry_by_id(c, id))
    else {
        st.emit(Event::Error("lore entry not found".into()));
        return;
    };
    let path = crate::lore::abs_path(&project.lore_root, &entry.rel_path);
    match std::fs::read_to_string(&path) {
        Ok(body) => st.emit(Event::LoreText { entry, body }),
        Err(e) => st.emit(Event::Error(format!("read {}: {e}", entry.rel_path))),
    }
}

// ---- Phase 5: Composite pipelines + burst ---------------------------------

struct ImageOut {
    asset_id: i64,
    bytes: Vec<u8>,
    content_type: String,
    seed: i64,
}

fn resolve_entity(entity: &str) -> String {
    let e = entity.trim();
    if e.is_empty() {
        "untitled".to_string()
    } else {
        e.to_string()
    }
}

/// Run one text→image generation through the local ComfyUI backend and store it
/// in the IP vault. Shared by Forge single-shot, burst, and the pipeline runner.
fn run_image_once(
    st: &mut State,
    project: &Project,
    req: &GenRequest,
    entity: &str,
) -> Result<ImageOut, String> {
    let params = serde_json::to_string(&serde_json::json!({
        "model": req.model, "negative": req.negative, "width": req.width,
        "height": req.height, "steps": req.steps, "cfg": req.cfg,
        "sampler": req.sampler, "scheduler": req.scheduler, "seed": req.seed
    }))
    .unwrap_or_default();
    let mut backend = backends::backend_for("comfy_local", &st.cfg.comfy_url);
    let job_id = match &st.project_conn {
        Some(c) => project::insert_job(c, "image", backend.id(), entity, &req.prompt, &params),
        None => 0,
    };
    refresh_forge(st);
    let ctx = st.ctx.clone();
    let tx = st.tx.clone();
    let res = backend.generate_image(req, &mut |frac, note| {
        let _ = tx.send(Event::Status(format!("forge: {note} {:.0}%", frac * 100.0)));
        ctx.request_repaint();
    });
    match res {
        Ok(gen) => {
            let ext = image_ext(&gen.content_type);
            let stem = format!("{entity}_{job_id}");
            let (id, pstr) = store_asset(
                st,
                project,
                "image",
                "images",
                &stem,
                ext,
                &gen.bytes,
                &gen.content_type,
                entity,
                &gen.meta,
                job_id,
            )?;
            if let Some(c) = &st.project_conn {
                project::update_job(
                    c,
                    job_id,
                    "done",
                    Some(&pstr),
                    &format!("seed {}", gen.seed),
                );
            }
            Ok(ImageOut {
                asset_id: id,
                bytes: gen.bytes,
                content_type: gen.content_type,
                seed: gen.seed,
            })
        }
        Err(e) => {
            if let Some(c) = &st.project_conn {
                project::update_job(c, job_id, "failed", None, &e);
            }
            Err(e)
        }
    }
}

/// Burst: generate `count` image variations. Seeds walk from the request's base
/// (or stay random when the base is < 0), so a burst spans the seed neighbourhood.
fn run_burst(st: &mut State, req: GenRequest, entity: String, count: u32) {
    let Some(project) = st.active_project.clone() else {
        st.emit(Event::Error("no project open — pick an IP first".into()));
        return;
    };
    if st.project_conn.is_none() {
        st.emit(Event::Error("project DB not open".into()));
        return;
    }
    let ent = resolve_entity(&entity);
    let n = count.clamp(1, 24);
    let base = req.seed;
    st.emit(Event::Busy(true));
    let mut ok = 0usize;
    for i in 0..n {
        let seed = if base < 0 { -1 } else { base + i as i64 };
        let r = GenRequest {
            seed,
            ..req.clone()
        };
        st.status(format!("burst {}/{n}", i + 1));
        match run_image_once(st, &project, &r, &ent) {
            Ok(o) => {
                ok += 1;
                st.emit(Event::Log(format!(
                    "✔ burst {}/{n} (seed {})",
                    i + 1,
                    o.seed
                )));
            }
            Err(e) => st.emit(Event::Log(format!("✘ burst {}/{n}: {e}", i + 1))),
        }
        st.emit(Event::Progress(i as usize + 1, n as usize, ok, 0));
    }
    st.emit(Event::Busy(false));
    st.status(format!("burst: {ok}/{n} images for {ent}"));
    refresh_forge(st);
    emit_project_info(st);
}

/// Copy the asset produced by an earlier stage into the IP engine tree under a
/// topic. Returns the destination path.
fn place_into_engine(
    st: &State,
    project: &Project,
    asset_id: i64,
    topic: &str,
) -> Result<String, String> {
    if project.engine_root.trim().is_empty() {
        return Err("no engine root set for this IP (Settings)".into());
    }
    let row = st
        .project_conn
        .as_ref()
        .and_then(|c| project::asset_by_id(c, asset_id))
        .ok_or("asset not found")?;
    let src = Path::new(&row.path);
    if !src.is_file() {
        return Err("asset file missing on disk".into());
    }
    let dir = Path::new(&project.engine_root)
        .join("Content")
        .join("Generated")
        .join(topic);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let dest = dir.join(&row.name);
    std::fs::copy(src, &dest).map_err(|e| e.to_string())?;
    let dp = dest.to_string_lossy().into_owned();
    if let Some(c) = &st.project_conn {
        project::set_engine_path(c, asset_id, &dp);
    }
    Ok(dp)
}

/// Run a named composite pipeline as a job graph, recording per-stage state in
/// `project.sqlite`. Stages whose backend isn't configured (or that need an
/// engine root) stop the run with a `blocked` state + reason rather than a
/// silent skip.
fn run_pipeline(st: &mut State, name: String, entity: String, req: GenRequest) {
    use crate::pipelines::{self, StageKind};
    let Some(project) = st.active_project.clone() else {
        st.emit(Event::Error("no project open — pick an IP first".into()));
        return;
    };
    if st.project_conn.is_none() {
        st.emit(Event::Error("project DB not open".into()));
        return;
    }
    let Some(def) = pipelines::by_name(&name) else {
        st.emit(Event::Error(format!("unknown pipeline: {name}")));
        return;
    };
    let ent = resolve_entity(&entity);
    let mut states = pipelines::initial_states(&def);
    let run_id = match &st.project_conn {
        Some(c) => {
            project::insert_pipeline(c, &name, &ent, &req.prompt, &pipelines::to_json(&states))
        }
        None => 0,
    };
    refresh_pipelines(st);
    st.emit(Event::Busy(true));

    let mut last_image: Option<(Vec<u8>, String)> = None;
    let mut last_asset: i64 = 0;
    let mut overall = "done";

    for i in 0..states.len() {
        states[i].status = "running".into();
        if let Some(c) = &st.project_conn {
            project::update_pipeline(c, run_id, "running", &pipelines::to_json(&states), "");
        }
        refresh_pipelines(st);
        st.status(format!("{name}: stage {}/{}", i + 1, states.len()));

        let outcome: Result<i64, String> =
            match states[i].kind {
                StageKind::Image => run_image_once(st, &project, &req, &ent).map(|o| {
                    last_image = Some((o.bytes, o.content_type));
                    last_asset = o.asset_id;
                    o.asset_id
                }),
                StageKind::Mesh => {
                    let tripo = backends::tripo::Tripo::new(&st.cfg.tripo_key);
                    match last_image.clone() {
                        _ if !tripo.configured() => Err("Tripo key not set (Settings)".into()),
                        None => Err("no upstream image to mesh".into()),
                        Some((bytes, ct)) => {
                            let ctx = st.ctx.clone();
                            let tx = st.tx.clone();
                            tripo
                            .image_to_mesh(&bytes, &ct, &mut |f, note| {
                                let _ = tx.send(Event::Status(format!(
                                    "tripo: {note} {:.0}%",
                                    f * 100.0
                                )));
                                ctx.request_repaint();
                            })
                            .and_then(|glb| {
                                let stem = format!("{ent}_{run_id}");
                                let meta = serde_json::json!({
                                    "backend": "tripo", "source": "image_to_model", "entity": ent
                                });
                                store_asset(
                                    st, &project, "mesh", "meshes", &stem, "glb", &glb,
                                    "model/gltf-binary", &ent, &meta, 0,
                                )
                                .map(|(id, _)| {
                                    last_asset = id;
                                    id
                                })
                            })
                        }
                    }
                }
                StageKind::Voice => {
                    let eleven = backends::audio::ElevenLabs::new(
                        &st.cfg.elevenlabs_key,
                        &st.cfg.elevenlabs_voice,
                    );
                    if !eleven.configured() {
                        Err("ElevenLabs key/voice not set (Settings)".into())
                    } else {
                        let ctx = st.ctx.clone();
                        let tx = st.tx.clone();
                        eleven
                            .text_to_speech(&req.prompt, &mut |f, note| {
                                let _ = tx.send(Event::Status(format!(
                                    "voice: {note} {:.0}%",
                                    f * 100.0
                                )));
                                ctx.request_repaint();
                            })
                            .and_then(|mp3| {
                                let stem = format!("{ent}_{run_id}");
                                let meta = serde_json::json!({
                                    "backend": "elevenlabs", "voice": st.cfg.elevenlabs_voice,
                                    "text": req.prompt, "entity": ent
                                });
                                store_asset(
                                    st,
                                    &project,
                                    "audio",
                                    "audio",
                                    &stem,
                                    "mp3",
                                    &mp3,
                                    "audio/mpeg",
                                    &ent,
                                    &meta,
                                    0,
                                )
                                .map(|(id, _)| {
                                    last_asset = id;
                                    id
                                })
                            })
                    }
                }
                StageKind::Place => {
                    if last_asset == 0 {
                        Err("no upstream asset to place".into())
                    } else {
                        place_into_engine(st, &project, last_asset, &states[i].topic)
                            .map(|_| last_asset)
                    }
                }
            };

        match outcome {
            Ok(id) => {
                states[i].status = "done".into();
                states[i].asset_id = id;
            }
            Err(e) => {
                let blocked = e.contains("not set") || e.contains("no engine");
                states[i].status = if blocked { "blocked" } else { "failed" }.into();
                states[i].detail = e.clone();
                overall = if blocked { "blocked" } else { "failed" };
                if let Some(c) = &st.project_conn {
                    project::update_pipeline(c, run_id, overall, &pipelines::to_json(&states), &e);
                }
                st.emit(Event::Log(format!("⏹ {name} stage {}: {e}", i + 1)));
                break;
            }
        }
        if let Some(c) = &st.project_conn {
            project::update_pipeline(c, run_id, "running", &pipelines::to_json(&states), "");
        }
    }

    if let Some(c) = &st.project_conn {
        project::update_pipeline(c, run_id, overall, &pipelines::to_json(&states), "");
    }
    st.emit(Event::Busy(false));
    st.status(format!("pipeline {name}: {overall}"));
    refresh_pipelines(st);
    refresh_forge(st);
    emit_project_info(st);
}

fn refresh_pipelines(st: &State) {
    if let Some(c) = &st.project_conn {
        st.emit(Event::Pipelines(project::recent_pipelines(c, 50)));
    }
}

// ---- Phase 6: Release authority --------------------------------------------

fn refresh_releases(st: &State) {
    if let Some(c) = &st.project_conn {
        st.emit(Event::Releases(project::recent_releases(c, 50)));
    }
}

/// Cut a freeze / ship-cut for the active IP: build the manifest (model-layer
/// snapshot + optional asset reproducibility trail), store it, and export it.
fn create_release(st: &mut State, name: String, kind: String) {
    let Some(ip) = st.active_project.clone() else {
        st.emit(Event::Error("no project open — pick an IP first".into()));
        return;
    };
    if st.project_conn.is_none() {
        st.emit(Event::Error("project DB not open".into()));
        return;
    }
    let nm = name.trim().to_string();
    if nm.is_empty() {
        st.emit(Event::Error("enter a release name".into()));
        return;
    }
    let kind = if kind == "shipcut" {
        "shipcut"
    } else {
        "freeze"
    };
    st.emit(Event::Busy(true));
    let (manifest, summary) = {
        let pc = st.project_conn.as_ref().unwrap();
        crate::release::build_manifest(kind, &nm, &ip, pc, st.conn.as_ref())
    };
    let manifest_str = manifest.to_string();
    let row_id = match &st.project_conn {
        Some(c) => project::insert_release(c, &nm, kind, &manifest_str),
        None => 0,
    };
    // export the just-cut manifest to the vault
    let exported = project::release_by_id(st.project_conn.as_ref().unwrap(), row_id)
        .and_then(|r| crate::release::export(&ip, &r).ok());
    st.emit(Event::Busy(false));
    match exported {
        Some(path) => st.status(format!(
            "{kind} '{nm}': {} assets, {} frozen models → {path}",
            summary.assets, summary.frozen_models
        )),
        None => st.status(format!(
            "{kind} '{nm}': {} assets, {} frozen models (export failed)",
            summary.assets, summary.frozen_models
        )),
    }
    st.emit(Event::Log(format!(
        "release '{nm}' [{kind}]: {} assets · {} frozen models · {} prompts · {} lore docs",
        summary.assets, summary.frozen_models, summary.prompts, summary.lore
    )));
    refresh_releases(st);
    emit_project_info(st);
}

fn export_release(st: &State, id: i64) {
    let Some(ip) = st.active_project.as_ref() else {
        st.emit(Event::Error("no project open".into()));
        return;
    };
    let Some(row) = st
        .project_conn
        .as_ref()
        .and_then(|c| project::release_by_id(c, id))
    else {
        st.emit(Event::Error("release not found".into()));
        return;
    };
    match crate::release::export(ip, &row) {
        Ok(path) => st.status(format!("exported {} → {path}", row.name)),
        Err(e) => st.emit(Event::Error(format!("export: {e}"))),
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
                    let page =
                        st.client
                            .models_page(t, b, sort, period, cfg.nsfw, 100, cursor.as_deref());
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
        Ok((Some(u), Some(n), mt, sha, mid)) => (u, n, mt.unwrap_or_default(), sha, mid),
        _ => {
            st.emit(Event::Error(format!("file {file_id} not downloadable")));
            return;
        }
    };
    let dest = vault_dest(&st.cfg, &mtype, &name);
    if dest.exists() {
        if let Some(conn) = &st.conn {
            db::set_downloaded(
                conn,
                file_id,
                &dest.to_string_lossy(),
                sha_expected.as_deref().unwrap_or(""),
            );
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
                    done >> 20,
                    total >> 20
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
                        st.emit(Event::Error(format!(
                            "SHA256 mismatch for {name} — discarded"
                        )));
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
    let Some(row) = db::file_row(conn, file_id) else {
        return;
    };
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
            db::log(
                conn,
                file_id,
                row.model_id,
                "promote",
                &dst.to_string_lossy(),
            );
            st.status(format!("active on NVMe: {}", row.file_name));
        }
        Err(e) => st.emit(Event::Error(format!("hotload failed: {e}"))),
    }
    refresh_manifest(st);
}

fn evict(st: &mut State, file_id: i64) {
    let Some(conn) = &st.conn else { return };
    let Some(row) = db::file_row(conn, file_id) else {
        return;
    };
    if row.locked {
        st.emit(Event::Error(
            "replica is locked — unlock before evicting".into(),
        ));
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

/// Capture example images + workflows for every tracked model (used after a heal
/// adopts files that never went through the download path).
fn harvest_all(st: &mut State) {
    let ids = match &st.conn {
        Some(c) => db::downloaded_model_ids(c),
        None => return,
    };
    if ids.is_empty() {
        st.status("no tracked models to capture images for");
        return;
    }
    st.emit(Event::Busy(true));
    let total = ids.len();
    let per = st.cfg.per_model;
    let mut saved = 0usize;
    let mut wf = 0usize;
    for (i, mid) in ids.iter().enumerate() {
        st.status(format!(
            "capturing images {}/{} (model {})",
            i + 1,
            total,
            mid
        ));
        let (s, w) = harvest_images(st, *mid, per, false);
        saved += s;
        wf += w;
        st.emit(Event::Progress(i + 1, total, saved, wf));
    }
    st.emit(Event::Busy(false));
    st.emit(Event::Log(format!(
        "── captured {saved} images ({wf} workflows) across {total} models ──"
    )));
    st.status(format!("captured {saved} images ({wf} workflows)"));
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Identify orphan files by SHA256 via CivitAI's by-hash endpoint, import the
/// model into the catalog, and adopt the file by exact hash match.
fn recover_orphans(st: &mut State, paths: Vec<String>) {
    if st.conn.is_none() {
        return;
    }
    st.emit(Event::Busy(true));
    let total = paths.len();
    let (mut recovered, mut notfound, mut errors) = (0usize, 0usize, 0usize);
    for (i, path) in paths.iter().enumerate() {
        let name = Path::new(path)
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or(path)
            .to_string();
        st.status(format!("hashing {}/{}: {}", i + 1, total, name));
        let hash = match sha256_file(Path::new(path)) {
            Ok(h) => h,
            Err(e) => {
                st.emit(Event::Log(format!("✘ hash {name}: {e}")));
                errors += 1;
                continue;
            }
        };
        match st.client.model_version_by_hash(&hash) {
            Ok(Some(ver)) => {
                let mid = ver.get("modelId").and_then(|x| x.as_i64());
                match mid {
                    Some(mid) => match st.client.model_by_id(mid) {
                        Ok(model) => {
                            if let Some(c) = &st.conn {
                                let _ = db::upsert_model(c, &model);
                                db::adopt_by_hash(c, &hash, path);
                                db::log(c, 0, mid, "recover", path);
                            }
                            st.emit(Event::Log(format!("✔ recovered {name} → model {mid}")));
                            recovered += 1;
                        }
                        Err(e) => {
                            st.emit(Event::Log(format!("✘ model {mid}: {e}")));
                            errors += 1;
                        }
                    },
                    None => {
                        st.emit(Event::Log(format!("? {name}: match without modelId")));
                        notfound += 1;
                    }
                }
            }
            Ok(None) => {
                st.emit(Event::Log(format!("– not on CivitAI: {name}")));
                notfound += 1;
            }
            Err(e) => {
                st.emit(Event::Log(format!("✘ lookup {name}: {e}")));
                errors += 1;
            }
        }
        st.emit(Event::Progress(i + 1, total, recovered, notfound));
    }
    st.emit(Event::Busy(false));
    st.emit(Event::Log(format!(
        "── recover: {recovered} adopted, {notfound} not on CivitAI, {errors} errors ──"
    )));
    st.status(format!(
        "recover: {recovered} adopted, {notfound} not found, {errors} errors"
    ));
    refresh_manifest(st);
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

/// Returns (images_saved, workflows_extracted).
fn harvest_images(st: &mut State, model_id: i64, per: u32, starter: bool) -> (usize, usize) {
    let extract = |raw: Option<&Value>| -> Vec<Value> {
        raw.and_then(|v| v.get("modelVersions"))
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|v0| v0.get("images"))
            .and_then(|i| i.as_array())
            .cloned()
            .unwrap_or_default()
    };
    // Prefer the stored model JSON; if it carries no example images (a model
    // adopted / reconciled / provisioned with only a stub raw), refetch it from
    // CivitAI by id so capture actually has something to pull. Negative ids are
    // non-CivitAI (HF / provisioned) and have no CivitAI gallery — skip those.
    let stored = st.conn.as_ref().and_then(|c| db::model_raw(c, model_id));
    let mut imgs = extract(stored.as_ref());
    if imgs.is_empty() && model_id > 0 {
        if let Ok(full) = st.client.model_by_id(model_id) {
            imgs = extract(Some(&full));
        }
    }
    if imgs.is_empty() {
        return (0, 0);
    }

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
    let mut saved = 0usize;
    let mut workflows = 0usize;
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
                workflows += 1;
            }
            if let Some(pr) = params {
                let p = mdir.join(format!("{img_id}.params.txt"));
                let _ = std::fs::write(&p, pr);
                params_path = Some(p.to_string_lossy().into_owned());
            }
        }
        if let Some(conn) = &st.conn {
            db::record_image(
                conn,
                img_id,
                model_id,
                &url,
                mtype,
                im.get("nsfwLevel").and_then(|x| x.as_str()),
                im.get("width").and_then(|x| x.as_i64()),
                im.get("height").and_then(|x| x.as_i64()),
                &fpath.to_string_lossy(),
                wf_path.as_deref(),
                params_path.as_deref(),
                has_wf,
                starter,
            );
        }
        saved += 1;
    }
    (saved, workflows)
}
