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
