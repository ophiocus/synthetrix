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

-- composite pipeline runs: a job graph over the media bus (Phase 5)
CREATE TABLE IF NOT EXISTS pipelines (
    id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, entity TEXT, notion TEXT,
    status TEXT, stages TEXT, detail TEXT,
    created_at TEXT DEFAULT (datetime('now')),
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
    pub engine_path: Option<String>,
    pub created_at: String,
}

/// Classify a file by extension: (kind, media_type). kind is a coarse bucket
/// used for the Asset Manager filters; media_type is a MIME-ish string.
pub fn media_type_for(path: &std::path::Path) -> (&'static str, String) {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => ("image", "image/png".into()),
        "jpg" | "jpeg" => ("image", "image/jpeg".into()),
        "webp" => ("image", "image/webp".into()),
        "gif" => ("image", "image/gif".into()),
        "mp4" | "mov" | "webm" | "mkv" => ("video", format!("video/{ext}")),
        "wav" => ("audio", "audio/wav".into()),
        "mp3" => ("audio", "audio/mpeg".into()),
        "flac" | "ogg" | "aac" | "m4a" => ("audio", format!("audio/{ext}")),
        "glb" | "gltf" => ("mesh", "model/gltf-binary".into()),
        "fbx" | "obj" | "usdz" | "usd" | "ply" | "stl" => ("mesh", format!("model/{ext}")),
        _ => ("other", "application/octet-stream".into()),
    }
}

fn row_to_asset(r: &rusqlite::Row) -> rusqlite::Result<AssetRow> {
    Ok(AssetRow {
        id: r.get(0)?,
        kind: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
        name: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
        entity: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
        media_type: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
        path: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
        engine_path: r.get(6)?,
        created_at: r.get::<_, Option<String>>(7)?.unwrap_or_default(),
    })
}

const ASSET_COLS: &str = "id,kind,name,entity,media_type,path,engine_path,created_at";

pub fn asset_exists(conn: &Connection, path: &str) -> bool {
    conn.query_row("SELECT 1 FROM assets WHERE path=?1 LIMIT 1", [path], |_| {
        Ok(())
    })
    .is_ok()
}

/// Register a file discovered by a vault scan (no originating job).
pub fn insert_scanned_asset(
    conn: &Connection,
    kind: &str,
    name: &str,
    entity: &str,
    media_type: &str,
    path: &str,
    sha256: &str,
) -> i64 {
    let _ = conn.execute(
        "INSERT INTO assets(kind,name,entity,media_type,path,sha256,job_id)
         VALUES(?1,?2,?3,?4,?5,?6,0)",
        rusqlite::params![kind, name, entity, media_type, path, sha256],
    );
    conn.last_insert_rowid()
}

pub fn set_engine_path(conn: &Connection, id: i64, engine_path: &str) {
    let _ = conn.execute(
        "UPDATE assets SET engine_path=?1 WHERE id=?2",
        rusqlite::params![engine_path, id],
    );
}

pub fn asset_by_id(conn: &Connection, id: i64) -> Option<AssetRow> {
    conn.query_row(
        &format!("SELECT {ASSET_COLS} FROM assets WHERE id=?1"),
        [id],
        row_to_asset,
    )
    .ok()
}

/// Filtered asset browse for the Asset Manager. `kind` filters the coarse bucket
/// (image/video/audio/mesh/other); `entity` is a substring match.
pub fn query_assets(
    conn: &Connection,
    kind: Option<&str>,
    entity: Option<&str>,
    limit: i64,
) -> Vec<AssetRow> {
    let mut sql = format!("SELECT {ASSET_COLS} FROM assets WHERE 1=1");
    if let Some(k) = kind {
        sql.push_str(&format!(" AND kind='{}'", k.replace('\'', "''")));
    }
    if let Some(e) = entity {
        sql.push_str(&format!(" AND entity LIKE '%{}%'", e.replace('\'', "''")));
    }
    sql.push_str(&format!(" ORDER BY id DESC LIMIT {limit}"));
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map([], row_to_asset)
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
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
    query_assets(conn, None, None, limit)
}

// ---- Prompts (the prompt storage matrix) ----------------------------------

#[derive(Clone, Default)]
pub struct PromptRow {
    pub id: i64,
    pub entity: String,
    pub slot: String,  // e.g. "Full Body Photoreal"
    pub stage: String, // "2d" | "video" | "voice" | "3d"
    pub backend: String,
    pub model: String,
    pub body: String,
    pub params: String,
    pub notes: String,
    pub updated_at: String,
}

fn row_to_prompt(r: &rusqlite::Row) -> rusqlite::Result<PromptRow> {
    Ok(PromptRow {
        id: r.get(0)?,
        entity: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
        slot: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
        stage: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
        backend: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
        model: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
        body: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
        params: r.get::<_, Option<String>>(7)?.unwrap_or_default(),
        notes: r.get::<_, Option<String>>(8)?.unwrap_or_default(),
        updated_at: r.get::<_, Option<String>>(9)?.unwrap_or_default(),
    })
}

const PROMPT_COLS: &str = "id,entity,slot,stage,backend,model,body,params,notes,updated_at";

pub fn query_prompts(conn: &Connection, entity: Option<&str>, limit: i64) -> Vec<PromptRow> {
    let mut sql = format!("SELECT {PROMPT_COLS} FROM prompts WHERE 1=1");
    if let Some(e) = entity {
        sql.push_str(&format!(" AND entity LIKE '%{}%'", e.replace('\'', "''")));
    }
    sql.push_str(&format!(" ORDER BY entity, slot LIMIT {limit}"));
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map([], row_to_prompt)
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

/// Insert (id==0) or update a prompt; returns its id.
pub fn upsert_prompt(conn: &Connection, p: &PromptRow) -> i64 {
    if p.id > 0 {
        let _ = conn.execute(
            "UPDATE prompts SET entity=?1,slot=?2,stage=?3,backend=?4,model=?5,
                body=?6,params=?7,notes=?8,updated_at=datetime('now') WHERE id=?9",
            rusqlite::params![
                p.entity, p.slot, p.stage, p.backend, p.model, p.body, p.params, p.notes, p.id
            ],
        );
        p.id
    } else {
        let _ = conn.execute(
            "INSERT INTO prompts(entity,slot,stage,backend,model,body,params,notes)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            rusqlite::params![
                p.entity, p.slot, p.stage, p.backend, p.model, p.body, p.params, p.notes
            ],
        );
        conn.last_insert_rowid()
    }
}

pub fn delete_prompt(conn: &Connection, id: i64) {
    let _ = conn.execute("DELETE FROM prompts WHERE id=?1", [id]);
}

/// Parse a per-entity `prompts.md` into prompt rows. Heuristic: `## <section>`
/// sets backend/stage context ("OpenArt … (Stage 2)" → openart/2d, "ElevenLabs"
/// → elevenlabs/voice, "Tripo" → tripo/3d); each `### <slot>` starts a prompt
/// whose body is the following prose, with a `**Model:** X` line pulled out.
pub fn parse_prompts_md(entity: &str, text: &str) -> Vec<PromptRow> {
    let mut out = Vec::new();
    let (mut backend, mut stage) = (String::new(), String::new());
    let mut cur: Option<PromptRow> = None;
    let mut body = String::new();
    let flush = |cur: &mut Option<PromptRow>, body: &mut String, out: &mut Vec<PromptRow>| {
        if let Some(mut p) = cur.take() {
            p.body = body.trim().to_string();
            if !p.body.is_empty() || !p.model.is_empty() {
                out.push(p);
            }
        }
        body.clear();
    };
    for line in text.lines() {
        let t = line.trim();
        if let Some(h) = t.strip_prefix("## ") {
            flush(&mut cur, &mut body, &mut out);
            let hl = h.to_lowercase();
            backend = if hl.contains("openart") {
                "openart".into()
            } else if hl.contains("elevenlabs") || hl.contains("voice") {
                "elevenlabs".into()
            } else if hl.contains("tripo") {
                "tripo".into()
            } else if hl.contains("comfy") {
                "comfy_local".into()
            } else {
                "".into()
            };
            stage = if hl.contains("voice") || hl.contains("elevenlabs") {
                "voice".into()
            } else if hl.contains("video") {
                "video".into()
            } else if hl.contains("3d") || hl.contains("tripo") {
                "3d".into()
            } else {
                "2d".into()
            };
        } else if let Some(s) = t.strip_prefix("### ") {
            flush(&mut cur, &mut body, &mut out);
            cur = Some(PromptRow {
                entity: entity.to_string(),
                slot: s.trim().to_string(),
                stage: stage.clone(),
                backend: backend.clone(),
                ..Default::default()
            });
        } else if let Some(m) = t.strip_prefix("**Model:**") {
            if let Some(p) = cur.as_mut() {
                p.model = m.split('|').next().unwrap_or("").trim().to_string();
            }
        } else if cur.is_some() && !t.starts_with("**") && !t.starts_with('#') {
            body.push_str(line);
            body.push('\n');
        }
    }
    flush(&mut cur, &mut body, &mut out);
    out
}

// ---- Pipelines (composite job graphs) -------------------------------------

#[derive(Clone, Default)]
pub struct PipelineRun {
    pub id: i64,
    pub name: String,
    pub entity: String,
    pub notion: String,
    pub status: String,
    /// JSON array of stage states: [{kind,backend,status,detail,asset_id}].
    pub stages: String,
    pub detail: String,
    pub updated_at: String,
}

/// Insert a new pipeline run in the "running" state; returns its id.
pub fn insert_pipeline(
    conn: &Connection,
    name: &str,
    entity: &str,
    notion: &str,
    stages: &str,
) -> i64 {
    let _ = conn.execute(
        "INSERT INTO pipelines(name,entity,notion,status,stages,detail)
         VALUES(?1,?2,?3,'running',?4,'')",
        rusqlite::params![name, entity, notion, stages],
    );
    conn.last_insert_rowid()
}

/// Update a run's overall status + the per-stage JSON snapshot.
pub fn update_pipeline(conn: &Connection, id: i64, status: &str, stages: &str, detail: &str) {
    let _ = conn.execute(
        "UPDATE pipelines SET status=?1, stages=?2, detail=?3, updated_at=datetime('now')
         WHERE id=?4",
        rusqlite::params![status, stages, detail, id],
    );
}

fn row_to_pipeline(r: &rusqlite::Row) -> rusqlite::Result<PipelineRun> {
    Ok(PipelineRun {
        id: r.get(0)?,
        name: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
        entity: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
        notion: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
        status: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
        stages: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
        detail: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
        updated_at: r.get::<_, Option<String>>(7)?.unwrap_or_default(),
    })
}

pub fn recent_pipelines(conn: &Connection, limit: i64) -> Vec<PipelineRun> {
    let mut stmt = match conn.prepare(
        "SELECT id,name,entity,notion,status,stages,detail,updated_at
         FROM pipelines ORDER BY id DESC LIMIT ?1",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map([limit], row_to_pipeline)
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

// ---- Releases (the IP's ship authority) -----------------------------------

#[derive(Clone, Default)]
pub struct ReleaseRow {
    pub id: i64,
    pub name: String,
    pub kind: String, // "freeze" | "shipcut"
    pub manifest: String,
    pub created_at: String,
}

pub fn insert_release(conn: &Connection, name: &str, kind: &str, manifest: &str) -> i64 {
    let _ = conn.execute(
        "INSERT INTO releases(name,kind,manifest) VALUES(?1,?2,?3)",
        rusqlite::params![name, kind, manifest],
    );
    conn.last_insert_rowid()
}

pub fn recent_releases(conn: &Connection, limit: i64) -> Vec<ReleaseRow> {
    let mut stmt = match conn
        .prepare("SELECT id,name,kind,manifest,created_at FROM releases ORDER BY id DESC LIMIT ?1")
    {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map([limit], |r| {
        Ok(ReleaseRow {
            id: r.get(0)?,
            name: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            kind: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            manifest: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            created_at: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
        })
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

pub fn release_by_id(conn: &Connection, id: i64) -> Option<ReleaseRow> {
    conn.query_row(
        "SELECT id,name,kind,manifest,created_at FROM releases WHERE id=?1",
        [id],
        |r| {
            Ok(ReleaseRow {
                id: r.get(0)?,
                name: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                kind: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                manifest: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                created_at: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
            })
        },
    )
    .ok()
}

/// Asset provenance rows for a release manifest: full reproducibility trail
/// (sha256 + originating job's prompt/params + engine placement).
pub fn release_asset_trail(conn: &Connection) -> Vec<serde_json::Value> {
    let mut stmt = match conn.prepare(
        "SELECT a.id, a.kind, a.name, a.entity, a.media_type, a.sha256, a.path,
                a.engine_path, a.metadata, j.prompt, j.params, j.backend
         FROM assets a LEFT JOIN jobs j ON j.id = a.job_id
         ORDER BY a.id",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map([], |r| {
        Ok(serde_json::json!({
            "id": r.get::<_, i64>(0)?,
            "kind": r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            "name": r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            "entity": r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            "media_type": r.get::<_, Option<String>>(4)?.unwrap_or_default(),
            "sha256": r.get::<_, Option<String>>(5)?.unwrap_or_default(),
            "path": r.get::<_, Option<String>>(6)?.unwrap_or_default(),
            "engine_path": r.get::<_, Option<String>>(7)?,
            "metadata": r.get::<_, Option<String>>(8)?.unwrap_or_default(),
            "job_prompt": r.get::<_, Option<String>>(9)?.unwrap_or_default(),
            "job_params": r.get::<_, Option<String>>(10)?.unwrap_or_default(),
            "job_backend": r.get::<_, Option<String>>(11)?.unwrap_or_default(),
        }))
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

/// Coarse asset counts by kind, for release summaries.
pub fn asset_kind_counts(conn: &Connection) -> Vec<(String, i64)> {
    let mut stmt =
        match conn.prepare("SELECT kind, COUNT(*) FROM assets GROUP BY kind ORDER BY kind") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
    stmt.query_map([], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?.unwrap_or_default(),
            r.get::<_, i64>(1)?,
        ))
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}
