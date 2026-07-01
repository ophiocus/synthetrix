//! Release authority (Phase 6). Synthetrix is the IP's authoritative release
//! entity: it cuts **freezes** (a model-layer snapshot — exactly which models,
//! by sha256, were active on the runtime when the cut was taken) and **ship-cuts**
//! (a full manifest of the IP's produced assets with their reproducibility
//! trail: asset → sha256 → originating job's prompt/params/backend → engine
//! placement). Manifests are stored in `project.sqlite` (releases table) and
//! can be exported as JSON for the vault / hand-off.

use crate::config::Project;
use crate::{db, project};
use rusqlite::Connection;
use serde_json::{json, Value};

/// A compact summary shown after a cut is taken.
pub struct ReleaseSummary {
    pub assets: usize,
    pub frozen_models: usize,
    pub prompts: i64,
    pub lore: i64,
}

/// The model-layer freeze: promoted (NVMe-active) models with their sha256, the
/// exact runtime set at cut time. Falls back to downloaded models if none are
/// promoted so a freeze is never empty on a fresh runtime.
fn model_freeze(catalog: Option<&Connection>) -> Vec<Value> {
    let Some(cat) = catalog else {
        return Vec::new();
    };
    let rows = match db::query_manifest(cat) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let promoted: Vec<&db::ManifestRow> = rows.iter().filter(|r| r.status == "promoted").collect();
    let chosen: Vec<&db::ManifestRow> = if promoted.is_empty() {
        rows.iter().collect()
    } else {
        promoted
    };
    chosen
        .iter()
        .map(|r| {
            json!({
                "model_id": r.model_id,
                "model": r.model_name,
                "type": r.model_type,
                "file": r.file_name,
                "sha256": r.sha256.clone().unwrap_or_default(),
                "size_kb": r.size_kb,
                "status": r.status,
                "locked": r.locked,
            })
        })
        .collect()
}

/// Build a release manifest. `kind` is "freeze" (model snapshot only) or
/// "shipcut" (model snapshot + full asset reproducibility trail).
pub fn build_manifest(
    kind: &str,
    name: &str,
    ip: &Project,
    project_conn: &Connection,
    catalog: Option<&Connection>,
) -> (Value, ReleaseSummary) {
    let stats = project::stats(project_conn);
    let frozen = model_freeze(catalog);
    let kind_counts: Vec<Value> = project::asset_kind_counts(project_conn)
        .into_iter()
        .map(|(k, n)| json!({ "kind": k, "count": n }))
        .collect();

    let mut manifest = json!({
        "release": name,
        "kind": kind,
        "ip": ip.name,
        "lore_root": ip.lore_root,
        "engine_root": ip.engine_root,
        "counts": {
            "assets": stats.assets,
            "prompts": stats.prompts,
            "lore": stats.lore,
            "jobs": stats.jobs,
        },
        "asset_kinds": kind_counts,
        "model_freeze": frozen,
    });

    // Ship-cuts carry the full per-asset reproducibility trail.
    if kind == "shipcut" {
        manifest["assets"] = Value::Array(project::release_asset_trail(project_conn));
    }

    let summary = ReleaseSummary {
        assets: stats.assets as usize,
        frozen_models: manifest["model_freeze"].as_array().map_or(0, |a| a.len()),
        prompts: stats.prompts,
        lore: stats.lore,
    };
    (manifest, summary)
}

/// Export a stored release manifest as pretty JSON under
/// `<lore_root>/.synthetrix/releases/<name>.json`. Returns the written path.
pub fn export(ip: &Project, row: &project::ReleaseRow) -> Result<String, String> {
    let dir = ip.synthetrix_dir().join("releases");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let safe: String = row
        .name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let path = dir.join(format!("{}-{}.json", row.kind, safe));
    // re-pretty the stored manifest; fall back to raw text if it isn't JSON
    let pretty = serde_json::from_str::<Value>(&row.manifest)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or_else(|| row.manifest.clone());
    std::fs::write(&path, pretty).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}
