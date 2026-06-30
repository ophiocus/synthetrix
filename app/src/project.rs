//! Per-IP project workspace. Each open IP gets a `project.sqlite` under
//! `<lore_root>/.synthetrix/` holding everything IP-scoped: lore index, prompt
//! matrix, generated-asset registry + metadata, the generation job queue, and
//! release state. The global *model* vault stays in the shared catalog.sqlite
//! (see db.rs) — models are reusable across IPs; everything here is not.

use rusqlite::Connection;
use std::path::Path;

const PROJECT_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT);

-- generation/asset jobs dispatched to backends (Phase 1)
CREATE TABLE IF NOT EXISTS jobs (
    id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT, status TEXT, backend TEXT,
    entity TEXT, notion TEXT, prompt TEXT, params TEXT, output_path TEXT,
    detail TEXT, created_at TEXT DEFAULT (datetime('now')),
    updated_at TEXT DEFAULT (datetime('now')));

-- the IP's digital vault: every produced item with metadata + provenance (Phase 2)
CREATE TABLE IF NOT EXISTS assets (
    id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT, name TEXT, entity TEXT,
    media_type TEXT, path TEXT, engine_path TEXT, sha256 TEXT, metadata TEXT,
    lore_ref TEXT, job_id INTEGER, stage TEXT, freeze TEXT,
    created_at TEXT DEFAULT (datetime('now')));

-- per-entity prompt matrix (Phase 3)
CREATE TABLE IF NOT EXISTS prompts (
    id INTEGER PRIMARY KEY AUTOINCREMENT, entity TEXT, slot TEXT, stage TEXT,
    backend TEXT, model TEXT, body TEXT, params TEXT, notes TEXT,
    updated_at TEXT DEFAULT (datetime('now')));

-- lore index over the lore-bible repo (Phase 4)
CREATE TABLE IF NOT EXISTS lore_index (
    id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT, name TEXT, rel_path TEXT,
    title TEXT, summary TEXT, vocab TEXT,
    updated_at TEXT DEFAULT (datetime('now')));

-- release state / freezes (Phase 6)
CREATE TABLE IF NOT EXISTS releases (
    id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, kind TEXT, manifest TEXT,
    created_at TEXT DEFAULT (datetime('now')));

CREATE INDEX IF NOT EXISTS idx_assets_entity ON assets(entity);
CREATE INDEX IF NOT EXISTS idx_jobs_status ON jobs(status);
CREATE INDEX IF NOT EXISTS idx_prompts_entity ON prompts(entity);
"#;

/// Open (creating + migrating) the project DB for an IP.
pub fn open(db_path: &Path, project_name: &str) -> Result<Connection, String> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let conn = Connection::open(db_path).map_err(|e| e.to_string())?;
    let _ = conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;");
    conn.execute_batch(PROJECT_SCHEMA)
        .map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO meta(key,value) VALUES('project',?1)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        [project_name],
    )
    .map_err(|e| e.to_string())?;
    Ok(conn)
}

/// Row counts for the dashboard.
#[derive(Clone, Default)]
pub struct ProjectStats {
    pub jobs: i64,
    pub assets: i64,
    pub prompts: i64,
    pub lore: i64,
}

pub fn stats(conn: &Connection) -> ProjectStats {
    let c = |sql: &str| conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(0);
    ProjectStats {
        jobs: c("SELECT COUNT(*) FROM jobs"),
        assets: c("SELECT COUNT(*) FROM assets"),
        prompts: c("SELECT COUNT(*) FROM prompts"),
        lore: c("SELECT COUNT(*) FROM lore_index"),
    }
}

// ---- Jobs (generation queue) ----------------------------------------------

#[derive(Clone)]
pub struct JobRow {
    pub id: i64,
    pub kind: String,
    pub status: String,
    pub backend: String,
    pub entity: String,
    pub prompt: String,
    pub output_path: Option<String>,
    pub detail: Option<String>,
    pub created_at: String,
}

/// Insert a queued job; returns its id.
pub fn insert_job(
    conn: &Connection,
    kind: &str,
    backend: &str,
    entity: &str,
    prompt: &str,
    params: &str,
) -> i64 {
    let _ = conn.execute(
        "INSERT INTO jobs(kind,status,backend,entity,prompt,params)
         VALUES(?1,'queued',?2,?3,?4,?5)",
        rusqlite::params![kind, backend, entity, prompt, params],
    );
    conn.last_insert_rowid()
}

pub fn update_job(conn: &Connection, id: i64, status: &str, output: Option<&str>, detail: &str) {
    let _ = conn.execute(
        "UPDATE jobs SET status=?1, output_path=COALESCE(?2,output_path),
            detail=?3, updated_at=datetime('now') WHERE id=?4",
        rusqlite::params![status, output, detail, id],
    );
}

pub fn recent_jobs(conn: &Connection, limit: i64) -> Vec<JobRow> {
    let mut stmt = match conn.prepare(
        "SELECT id,kind,status,backend,entity,prompt,output_path,detail,created_at
         FROM jobs ORDER BY id DESC LIMIT ?1",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map([limit], |r| {
        Ok(JobRow {
            id: r.get(0)?,
            kind: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            status: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            backend: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            entity: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
            prompt: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
            output_path: r.get(6)?,
            detail: r.get(7)?,
            created_at: r.get::<_, Option<String>>(8)?.unwrap_or_default(),
        })
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

// ---- Assets (the IP's digital vault) --------------------------------------

#[derive(Clone)]
pub struct AssetRow {
    pub id: i64,
    pub kind: String,
    pub name: String,
    pub entity: String,
    pub media_type: String,
    pub path: String,
    pub created_at: String,
}

#[allow(clippy::too_many_arguments)]
pub fn insert_asset(
    conn: &Connection,
    kind: &str,
    name: &str,
    entity: &str,
    media_type: &str,
    path: &str,
    sha256: &str,
    metadata: &str,
    job_id: i64,
) -> i64 {
    let _ = conn.execute(
        "INSERT INTO assets(kind,name,entity,media_type,path,sha256,metadata,job_id)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        rusqlite::params![kind, name, entity, media_type, path, sha256, metadata, job_id],
    );
    conn.last_insert_rowid()
}

pub fn recent_assets(conn: &Connection, limit: i64) -> Vec<AssetRow> {
    let mut stmt = match conn.prepare(
        "SELECT id,kind,name,entity,media_type,path,created_at
         FROM assets ORDER BY id DESC LIMIT ?1",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map([limit], |r| {
        Ok(AssetRow {
            id: r.get(0)?,
            kind: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            name: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            entity: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            media_type: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
            path: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
            created_at: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
        })
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}
