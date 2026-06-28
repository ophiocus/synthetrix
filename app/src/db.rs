//! The local manifest: SQLite registry shared with the Python harvester.
//! Models / versions / files / images, plus a `locked` pin flag and a `reflog`
//! state-transition log. The Manifest tab is a view over this.

use rusqlite::{params, Connection};
use serde_json::Value;
use std::path::{Path, PathBuf};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS models (
    model_id INTEGER PRIMARY KEY, name TEXT, type TEXT, nsfw INTEGER,
    creator TEXT, tags TEXT, downloads INTEGER, rating REAL, thumbs_up INTEGER,
    comments INTEGER, cover_url TEXT, raw TEXT);
CREATE TABLE IF NOT EXISTS versions (
    version_id INTEGER PRIMARY KEY, model_id INTEGER, name TEXT, base_model TEXT,
    published_at TEXT, trained_words TEXT, description TEXT, downloads INTEGER,
    version_idx INTEGER);
CREATE TABLE IF NOT EXISTS files (
    file_id INTEGER PRIMARY KEY, version_id INTEGER, name TEXT, type TEXT,
    size_kb REAL, download_url TEXT, sha256 TEXT, autov2 TEXT, fp TEXT,
    format TEXT, is_primary INTEGER, local_path TEXT, nvme_path TEXT,
    locked INTEGER DEFAULT 0, status TEXT DEFAULT 'indexed');
CREATE TABLE IF NOT EXISTS images (
    image_id INTEGER PRIMARY KEY, model_id INTEGER, url TEXT, media_type TEXT,
    nsfw_level TEXT, width INTEGER, height INTEGER, reactions INTEGER,
    local_path TEXT, workflow_path TEXT, params_path TEXT,
    has_workflow INTEGER DEFAULT 0, is_starter INTEGER DEFAULT 0,
    status TEXT DEFAULT 'indexed');
CREATE TABLE IF NOT EXISTS reflog (
    id INTEGER PRIMARY KEY AUTOINCREMENT, file_id INTEGER, model_id INTEGER,
    action TEXT, detail TEXT, ts TEXT DEFAULT (datetime('now')));
CREATE INDEX IF NOT EXISTS idx_files_ver ON files(version_id);
CREATE INDEX IF NOT EXISTS idx_versions_mod ON versions(model_id);
CREATE INDEX IF NOT EXISTS idx_images_model ON images(model_id);
CREATE INDEX IF NOT EXISTS idx_models_type ON models(type);
"#;

pub fn open(catalog_dir: &str) -> Result<Connection, String> {
    let dir = Path::new(catalog_dir);
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let conn = Connection::open(dir.join("catalog.sqlite")).map_err(|e| e.to_string())?;
    // The catalog often lives on a spinning HDD; WAL + NORMAL turns per-row
    // fsync storms into a single periodic flush. Huge win for bulk sync.
    let _ = conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA temp_store=MEMORY;",
    );
    conn.execute_batch(SCHEMA).map_err(|e| e.to_string())?;
    // migration guards for DBs created by older builds / the Python tool
    for (table, col, decl) in [
        ("files", "locked", "INTEGER DEFAULT 0"),
        ("images", "is_starter", "INTEGER DEFAULT 0"),
        ("models", "cover_url", "TEXT"),
    ] {
        let has: bool = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .and_then(|mut s| {
                s.query_map([], |r| r.get::<_, String>(1))
                    .map(|rows| rows.filter_map(|r| r.ok()).any(|n| n == col))
            })
            .unwrap_or(true);
        if !has {
            let _ = conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {col} {decl}"), []);
        }
    }
    Ok(conn)
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(|x| x.as_str()).map(|x| x.to_string())
}
fn i(v: &Value, k: &str) -> Option<i64> {
    v.get(k).and_then(|x| x.as_i64())
}

/// Upsert a full model object (matches the Python catalog schema).
pub fn upsert_model(conn: &Connection, m: &Value) -> Result<(), String> {
    let id = i(m, "id").ok_or("model has no id")?;
    let stats = m.get("stats").cloned().unwrap_or(Value::Null);
    // first still-image URL of the latest version = the list cover (no download)
    let cover = m
        .get("modelVersions")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|v0| v0.get("images"))
        .and_then(|i| i.as_array())
        .and_then(|a| {
            a.iter()
                .find(|im| im.get("type").and_then(|t| t.as_str()) == Some("image"))
                .or_else(|| a.first())
        })
        .and_then(|im| im.get("url"))
        .and_then(|u| u.as_str())
        // CivitAI CDN honors width transforms — request a thumbnail, not the
        // multi-MB original, for the Picker list.
        .map(|s| s.replace("original=true", "width=256"));
    conn.execute(
        "INSERT INTO models(model_id,name,type,nsfw,creator,tags,downloads,rating,
            thumbs_up,comments,cover_url,raw) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
         ON CONFLICT(model_id) DO UPDATE SET name=excluded.name,
            downloads=excluded.downloads, rating=excluded.rating,
            thumbs_up=excluded.thumbs_up, comments=excluded.comments,
            tags=excluded.tags, cover_url=excluded.cover_url, raw=excluded.raw",
        params![
            id,
            s(m, "name"),
            s(m, "type"),
            m.get("nsfw").and_then(|x| x.as_bool()).unwrap_or(false) as i64,
            m.get("creator").and_then(|c| c.get("username")).and_then(|u| u.as_str()),
            m.get("tags").map(|t| t.to_string()),
            stats.get("downloadCount").and_then(|x| x.as_i64()),
            stats.get("rating").and_then(|x| x.as_f64()),
            stats.get("thumbsUpCount").and_then(|x| x.as_i64()),
            stats.get("commentCount").and_then(|x| x.as_i64()),
            cover,
            m.to_string(),
        ],
    )
    .map_err(|e| e.to_string())?;

    if let Some(versions) = m.get("modelVersions").and_then(|v| v.as_array()) {
        for (idx, v) in versions.iter().enumerate() {
            let vid = match i(v, "id") {
                Some(x) => x,
                None => continue,
            };
            let vstats = v.get("stats").cloned().unwrap_or(Value::Null);
            conn.execute(
                "INSERT INTO versions(version_id,model_id,name,base_model,published_at,
                    trained_words,description,downloads,version_idx)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)
                 ON CONFLICT(version_id) DO UPDATE SET base_model=excluded.base_model,
                    version_idx=excluded.version_idx, trained_words=excluded.trained_words,
                    description=excluded.description, downloads=excluded.downloads",
                params![
                    vid, id,
                    s(v, "name"),
                    s(v, "baseModel"),
                    s(v, "publishedAt"),
                    v.get("trainedWords").map(|t| t.to_string()),
                    s(v, "description"),
                    vstats.get("downloadCount").and_then(|x| x.as_i64()),
                    idx as i64,
                ],
            )
            .map_err(|e| e.to_string())?;

            if let Some(files) = v.get("files").and_then(|f| f.as_array()) {
                for f in files {
                    let fid = match i(f, "id") {
                        Some(x) => x,
                        None => continue,
                    };
                    let hashes = f.get("hashes").cloned().unwrap_or(Value::Null);
                    let meta = f.get("metadata").cloned().unwrap_or(Value::Null);
                    conn.execute(
                        "INSERT INTO files(file_id,version_id,name,type,size_kb,
                            download_url,sha256,autov2,fp,format,is_primary)
                         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
                         ON CONFLICT(file_id) DO UPDATE SET
                            download_url=excluded.download_url, size_kb=excluded.size_kb,
                            sha256=excluded.sha256",
                        params![
                            fid, vid,
                            s(f, "name"),
                            s(f, "type"),
                            f.get("sizeKB").and_then(|x| x.as_f64()),
                            s(f, "downloadUrl"),
                            hashes.get("SHA256").and_then(|x| x.as_str()),
                            hashes.get("AutoV2").and_then(|x| x.as_str()),
                            meta.get("fp").and_then(|x| x.as_str()),
                            meta.get("format").and_then(|x| x.as_str()),
                            f.get("primary").and_then(|x| x.as_bool()).unwrap_or(false) as i64,
                        ],
                    )
                    .map_err(|e| e.to_string())?;
                }
            }
        }
    }
    Ok(())
}

pub fn model_raw(conn: &Connection, model_id: i64) -> Option<Value> {
    conn.query_row("SELECT raw FROM models WHERE model_id=?1", [model_id], |r| {
        r.get::<_, String>(0)
    })
    .ok()
    .and_then(|s| serde_json::from_str(&s).ok())
}

// ---- Picker ----------------------------------------------------------------

#[derive(Clone, Default)]
pub struct PickFilter {
    pub model_type: Option<String>,
    pub base: Option<String>,
    pub search: Option<String>,
    pub status: Option<String>,
    pub min_downloads: i64,
    pub limit: i64,
}

#[derive(Clone)]
pub struct PickRow {
    pub file_id: i64,
    pub model_id: i64,
    pub model_name: String,
    pub model_type: String,
    pub base_model: String,
    pub nsfw: bool,
    pub downloads: i64,
    pub rating: f64,
    pub size_kb: f64,
    pub trained_words: String,
    pub status: String,
    pub locked: bool,
    pub cover_url: Option<String>,
}

pub fn query_picks(conn: &Connection, f: &PickFilter) -> Result<Vec<PickRow>, String> {
    let mut sql = String::from(
        "SELECT f.file_id, m.model_id, m.name, m.type, v.base_model, m.nsfw,
                m.downloads, m.rating, f.size_kb, v.trained_words, f.status, f.locked,
                m.cover_url
         FROM models m
         JOIN versions v ON v.model_id=m.model_id AND v.version_idx=0
         JOIN files f ON f.version_id=v.version_id AND f.is_primary=1
         WHERE 1=1",
    );
    if let Some(t) = &f.model_type {
        sql.push_str(&format!(" AND m.type='{}'", t.replace('\'', "''")));
    }
    if let Some(b) = &f.base {
        sql.push_str(&format!(" AND v.base_model LIKE '%{}%'", b.replace('\'', "''")));
    }
    if let Some(q) = &f.search {
        sql.push_str(&format!(" AND m.name LIKE '%{}%'", q.replace('\'', "''")));
    }
    if let Some(st) = &f.status {
        sql.push_str(&format!(" AND f.status='{}'", st.replace('\'', "''")));
    }
    if f.min_downloads > 0 {
        sql.push_str(&format!(" AND m.downloads >= {}", f.min_downloads));
    }
    sql.push_str(" ORDER BY m.downloads DESC");
    if f.limit > 0 {
        sql.push_str(&format!(" LIMIT {}", f.limit));
    }

    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok(PickRow {
                file_id: r.get(0)?,
                model_id: r.get(1)?,
                model_name: r.get(2)?,
                model_type: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                base_model: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                nsfw: r.get::<_, i64>(5)? != 0,
                downloads: r.get::<_, Option<i64>>(6)?.unwrap_or(0),
                rating: r.get::<_, Option<f64>>(7)?.unwrap_or(0.0),
                size_kb: r.get::<_, Option<f64>>(8)?.unwrap_or(0.0),
                trained_words: r.get::<_, Option<String>>(9)?.unwrap_or_default(),
                status: r.get::<_, Option<String>>(10)?.unwrap_or_else(|| "indexed".into()),
                locked: r.get::<_, i64>(11)? != 0,
                cover_url: r.get::<_, Option<String>>(12)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

// ---- Manifest --------------------------------------------------------------

#[derive(Clone)]
pub struct ManifestRow {
    pub file_id: i64,
    pub model_id: i64,
    pub model_name: String,
    pub model_type: String,
    pub file_name: String,
    pub size_kb: f64,
    pub sha256: Option<String>,
    pub local_path: Option<String>,
    pub nvme_path: Option<String>,
    pub status: String,
    pub locked: bool,
}

pub fn query_manifest(conn: &Connection) -> Result<Vec<ManifestRow>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT f.file_id, v.model_id, m.name, m.type, f.name, f.size_kb,
                    f.sha256, f.local_path, f.nvme_path, f.status, f.locked
             FROM files f JOIN versions v ON v.version_id=f.version_id
             JOIN models m ON m.model_id=v.model_id
             WHERE f.status IN ('downloaded','promoted')
             ORDER BY f.locked DESC, m.name",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok(ManifestRow {
                file_id: r.get(0)?,
                model_id: r.get(1)?,
                model_name: r.get(2)?,
                model_type: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                file_name: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                size_kb: r.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
                sha256: r.get(6)?,
                local_path: r.get(7)?,
                nvme_path: r.get(8)?,
                status: r.get::<_, Option<String>>(9)?.unwrap_or_else(|| "indexed".into()),
                locked: r.get::<_, i64>(10)? != 0,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

pub fn file_row(conn: &Connection, file_id: i64) -> Option<ManifestRow> {
    conn.query_row(
        "SELECT f.file_id, v.model_id, m.name, m.type, f.name, f.size_kb,
                f.sha256, f.local_path, f.nvme_path, f.status, f.locked
         FROM files f JOIN versions v ON v.version_id=f.version_id
         JOIN models m ON m.model_id=v.model_id WHERE f.file_id=?1",
        [file_id],
        |r| {
            Ok(ManifestRow {
                file_id: r.get(0)?,
                model_id: r.get(1)?,
                model_name: r.get(2)?,
                model_type: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                file_name: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                size_kb: r.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
                sha256: r.get(6)?,
                local_path: r.get(7)?,
                nvme_path: r.get(8)?,
                status: r.get::<_, Option<String>>(9)?.unwrap_or_else(|| "indexed".into()),
                locked: r.get::<_, i64>(10)? != 0,
            })
        },
    )
    .ok()
}

// ---- Mutations -------------------------------------------------------------

pub fn log(conn: &Connection, file_id: i64, model_id: i64, action: &str, detail: &str) {
    let _ = conn.execute(
        "INSERT INTO reflog(file_id,model_id,action,detail) VALUES(?1,?2,?3,?4)",
        params![file_id, model_id, action, detail],
    );
}

pub fn set_downloaded(conn: &Connection, file_id: i64, local_path: &str, sha: &str) {
    let _ = conn.execute(
        "UPDATE files SET local_path=?1, sha256=COALESCE(NULLIF(sha256,''),?2),
            status='downloaded' WHERE file_id=?3",
        params![local_path, sha, file_id],
    );
}

pub fn set_promoted(conn: &Connection, file_id: i64, nvme_path: &str) {
    let _ = conn.execute(
        "UPDATE files SET nvme_path=?1, status='promoted' WHERE file_id=?2",
        params![nvme_path, file_id],
    );
}

pub fn set_evicted(conn: &Connection, file_id: i64) {
    let _ = conn.execute(
        "UPDATE files SET nvme_path=NULL, status='downloaded' WHERE file_id=?1",
        [file_id],
    );
}

pub fn set_locked(conn: &Connection, file_id: i64, locked: bool) {
    let _ = conn.execute(
        "UPDATE files SET locked=?1 WHERE file_id=?2",
        params![locked as i64, file_id],
    );
}

pub fn record_image(
    conn: &Connection,
    image_id: i64,
    model_id: i64,
    url: &str,
    media_type: &str,
    nsfw_level: Option<&str>,
    width: Option<i64>,
    height: Option<i64>,
    local_path: &str,
    workflow_path: Option<&str>,
    params_path: Option<&str>,
    has_workflow: bool,
    is_starter: bool,
) {
    let _ = conn.execute(
        "INSERT INTO images(image_id,model_id,url,media_type,nsfw_level,width,height,
            reactions,local_path,workflow_path,params_path,has_workflow,is_starter,status)
         VALUES(?1,?2,?3,?4,?5,?6,?7,NULL,?8,?9,?10,?11,?12,'saved')
         ON CONFLICT(image_id) DO UPDATE SET local_path=excluded.local_path,
            workflow_path=excluded.workflow_path, params_path=excluded.params_path,
            has_workflow=excluded.has_workflow, status='saved'",
        params![
            image_id, model_id, url, media_type, nsfw_level, width, height,
            local_path, workflow_path, params_path, has_workflow as i64, is_starter as i64
        ],
    );
}

pub fn image_exists(conn: &Connection, image_id: i64) -> Option<String> {
    conn.query_row(
        "SELECT local_path FROM images WHERE image_id=?1",
        [image_id],
        |r| r.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
}

// ---- Audit / heal ----------------------------------------------------------

#[derive(Clone, Default)]
pub struct AuditReport {
    pub checked: usize,
    pub missing_vault: Vec<(i64, String)>, // status=downloaded but file gone
    pub missing_nvme: Vec<(i64, String)>,  // status=promoted but nvme replica gone
    pub orphans: Vec<String>,              // files on disk under vault, not in manifest
}

pub fn audit(conn: &Connection, vault_root: &str) -> Result<AuditReport, String> {
    let mut rep = AuditReport::default();
    let rows = query_manifest(conn)?;
    let mut known: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for r in &rows {
        rep.checked += 1;
        if let Some(lp) = &r.local_path {
            known.insert(PathBuf::from(lp));
            if !Path::new(lp).exists() {
                rep.missing_vault.push((r.file_id, lp.clone()));
            }
        }
        if r.status == "promoted" {
            match &r.nvme_path {
                Some(np) if Path::new(np).exists() => {}
                _ => rep
                    .missing_nvme
                    .push((r.file_id, r.nvme_path.clone().unwrap_or_default())),
            }
        }
    }
    // orphan scan: model files in the vault subdirs not referenced by the manifest
    for sub in ["checkpoints", "loras", "embeddings", "controlnet", "vae", "upscale_models"] {
        let dir = Path::new(vault_root).join(sub);
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_file() {
                    let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("");
                    if matches!(ext, "safetensors" | "ckpt" | "pt" | "bin" | "pth")
                        && !known.contains(&p)
                    {
                        rep.orphans.push(p.to_string_lossy().into_owned());
                    }
                }
            }
        }
    }
    Ok(rep)
}

/// Heal the safe cases: reset manifest rows whose files vanished so they can be
/// re-fetched. Returns count reset. (Orphans are reported, never auto-deleted.)
pub fn heal(conn: &Connection, rep: &AuditReport) -> usize {
    let mut n = 0;
    for (fid, _) in &rep.missing_vault {
        let _ = conn.execute(
            "UPDATE files SET status='indexed', local_path=NULL, nvme_path=NULL
             WHERE file_id=?1",
            [fid],
        );
        log(conn, *fid, 0, "heal", "vault file missing -> reset to indexed");
        n += 1;
    }
    for (fid, _) in &rep.missing_nvme {
        let _ = conn.execute(
            "UPDATE files SET status='downloaded', nvme_path=NULL WHERE file_id=?1",
            [fid],
        );
        log(conn, *fid, 0, "heal", "nvme replica missing -> demote to downloaded");
        n += 1;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c
    }

    #[test]
    fn upsert_and_query_roundtrip() {
        let conn = mem();
        let model = serde_json::json!({
            "id": 4201, "name": "Test Checkpoint", "type": "Checkpoint",
            "nsfw": true, "stats": {"downloadCount": 1234, "rating": 4.8},
            "modelVersions": [{
                "id": 999, "name": "v1", "baseModel": "SDXL 1.0",
                "trainedWords": ["zxc"],
                "files": [{
                    "id": 555, "name": "test.safetensors", "type": "Model",
                    "sizeKB": 6_500_000.0, "primary": true,
                    "downloadUrl": "https://example/d/555",
                    "hashes": {"SHA256": "ABCDEF"}
                }]
            }]
        });
        upsert_model(&conn, &model).unwrap();

        let rows = query_picks(&conn, &PickFilter { limit: 50, ..Default::default() }).unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.file_id, 555);
        assert_eq!(r.model_id, 4201);
        assert_eq!(r.model_type, "Checkpoint");
        assert_eq!(r.base_model, "SDXL 1.0");
        assert!(r.nsfw);
        assert_eq!(r.status, "indexed");

        // state transitions reflected in manifest. The catalog hash (ABCDEF)
        // is authoritative; set_downloaded only fills it when previously empty.
        set_downloaded(&conn, 555, "H:/Models/checkpoints/test.safetensors", "abcdef");
        set_promoted(&conn, 555, "E:/.../test.safetensors");
        set_locked(&conn, 555, true);
        let man = query_manifest(&conn).unwrap();
        assert_eq!(man.len(), 1);
        assert_eq!(man[0].status, "promoted");
        assert!(man[0].locked);
        assert_eq!(man[0].sha256.as_deref(), Some("ABCDEF"));
    }
}
