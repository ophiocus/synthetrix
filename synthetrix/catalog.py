"""SQLite catalog: the local index that drives harvesting choices.

Stores model / version / file rows plus a denormalized view for fast picking,
and writes a human-readable usage-doc sidecar (.md) per model version.
"""
from __future__ import annotations

import json
import re
import sqlite3
from html import unescape
from pathlib import Path

SCHEMA = """
CREATE TABLE IF NOT EXISTS models (
    model_id    INTEGER PRIMARY KEY,
    name        TEXT,
    type        TEXT,
    nsfw        INTEGER,
    creator     TEXT,
    tags        TEXT,            -- json array
    downloads   INTEGER,
    rating      REAL,
    thumbs_up   INTEGER,
    comments    INTEGER,
    raw         TEXT             -- full json
);
CREATE TABLE IF NOT EXISTS versions (
    version_id    INTEGER PRIMARY KEY,
    model_id      INTEGER REFERENCES models(model_id),
    name          TEXT,
    base_model    TEXT,
    published_at  TEXT,
    trained_words TEXT,          -- json array
    description   TEXT,          -- raw html
    downloads     INTEGER,
    version_idx   INTEGER        -- 0 = latest version of the model
);
CREATE TABLE IF NOT EXISTS files (
    file_id      INTEGER PRIMARY KEY,
    version_id   INTEGER REFERENCES versions(version_id),
    name         TEXT,
    type         TEXT,
    size_kb      REAL,
    download_url TEXT,
    sha256       TEXT,
    autov2       TEXT,
    fp           TEXT,
    format       TEXT,
    is_primary   INTEGER,
    local_path   TEXT,           -- set when downloaded to vault
    nvme_path    TEXT,           -- set when promoted to NVMe
    status       TEXT DEFAULT 'indexed'  -- indexed | downloaded | promoted
);
CREATE TABLE IF NOT EXISTS images (
    image_id      INTEGER PRIMARY KEY,
    model_id      INTEGER REFERENCES models(model_id),
    url           TEXT,
    media_type    TEXT,            -- image | video
    nsfw_level    TEXT,
    width         INTEGER,
    height        INTEGER,
    reactions     INTEGER,
    local_path    TEXT,            -- downloaded original
    workflow_path TEXT,            -- extracted ComfyUI workflow json (PNG only)
    params_path   TEXT,            -- extracted A1111 parameters / prompt (PNG only)
    has_workflow  INTEGER DEFAULT 0,
    is_starter    INTEGER DEFAULT 0,      -- 1 = the single index-time preview
    status        TEXT DEFAULT 'indexed'  -- indexed | saved
);
CREATE INDEX IF NOT EXISTS idx_images_model ON images(model_id);
CREATE INDEX IF NOT EXISTS idx_files_sha   ON files(sha256);
CREATE INDEX IF NOT EXISTS idx_files_ver   ON files(version_id);
CREATE INDEX IF NOT EXISTS idx_versions_mod ON versions(model_id);
CREATE INDEX IF NOT EXISTS idx_models_type ON models(type);

-- Convenience view for picking: one row per primary file of the latest version.
CREATE VIEW IF NOT EXISTS picks AS
SELECT m.model_id, m.name AS model_name, m.type, m.nsfw, m.creator,
       m.downloads, m.rating, m.thumbs_up,
       v.version_id, v.base_model, v.trained_words, v.published_at,
       f.file_id, f.name AS file_name, f.size_kb, f.sha256,
       f.local_path, f.nvme_path, f.status
FROM models m
JOIN versions v ON v.model_id = m.model_id AND v.version_idx = 0
JOIN files f    ON f.version_id = v.version_id AND f.is_primary = 1;
"""


def connect(catalog_dir: str | Path) -> sqlite3.Connection:
    d = Path(catalog_dir)
    d.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(d / "catalog.sqlite")
    conn.row_factory = sqlite3.Row
    conn.executescript(SCHEMA)
    # migration guard: add is_starter to a pre-existing images table
    cols = {r["name"] for r in conn.execute("PRAGMA table_info(images)")}
    if "is_starter" not in cols:
        conn.execute("ALTER TABLE images ADD COLUMN is_starter INTEGER DEFAULT 0")
        conn.commit()
    return conn


def known_model_ids(conn: sqlite3.Connection) -> list[int]:
    """Every model_id currently in the catalog — the set the delta crawl and
    the ids-refresh pass diff against."""
    return [r["model_id"] for r in conn.execute("SELECT model_id FROM models")]


def _hash(file_obj: dict, kind: str) -> str | None:
    return (file_obj.get("hashes") or {}).get(kind)


def upsert_model(conn: sqlite3.Connection, m: dict) -> None:
    stats = m.get("stats") or {}
    conn.execute(
        """INSERT INTO models(model_id,name,type,nsfw,creator,tags,downloads,
                              rating,thumbs_up,comments,raw)
           VALUES(?,?,?,?,?,?,?,?,?,?,?)
           ON CONFLICT(model_id) DO UPDATE SET
             name=excluded.name, downloads=excluded.downloads,
             rating=excluded.rating, thumbs_up=excluded.thumbs_up,
             comments=excluded.comments, tags=excluded.tags, raw=excluded.raw""",
        (
            m["id"], m.get("name"), m.get("type"), int(bool(m.get("nsfw"))),
            (m.get("creator") or {}).get("username"),
            json.dumps([t for t in (m.get("tags") or [])]),
            stats.get("downloadCount"), stats.get("rating"),
            stats.get("thumbsUpCount"), stats.get("commentCount"),
            json.dumps(m),
        ),
    )
    for idx, v in enumerate(m.get("modelVersions") or []):
        vstats = v.get("stats") or {}
        conn.execute(
            """INSERT INTO versions(version_id,model_id,name,base_model,
                   published_at,trained_words,description,downloads,version_idx)
               VALUES(?,?,?,?,?,?,?,?,?)
               ON CONFLICT(version_id) DO UPDATE SET
                 base_model=excluded.base_model, version_idx=excluded.version_idx,
                 trained_words=excluded.trained_words,
                 description=excluded.description, downloads=excluded.downloads""",
            (
                v["id"], m["id"], v.get("name"), v.get("baseModel"),
                v.get("publishedAt"),
                json.dumps(v.get("trainedWords") or []),
                v.get("description"), vstats.get("downloadCount"), idx,
            ),
        )
        for f in v.get("files") or []:
            meta = f.get("metadata") or {}
            conn.execute(
                """INSERT INTO files(file_id,version_id,name,type,size_kb,
                       download_url,sha256,autov2,fp,format,is_primary)
                   VALUES(?,?,?,?,?,?,?,?,?,?,?)
                   ON CONFLICT(file_id) DO UPDATE SET
                     download_url=excluded.download_url, size_kb=excluded.size_kb,
                     sha256=excluded.sha256""",
                (
                    f["id"], v["id"], f.get("name"), f.get("type"),
                    f.get("sizeKB"), f.get("downloadUrl"),
                    _hash(f, "SHA256"), _hash(f, "AutoV2"),
                    meta.get("fp"), meta.get("format"),
                    int(bool(f.get("primary"))),
                ),
            )
    conn.commit()


def _html_to_text(html: str | None) -> str:
    if not html:
        return ""
    text = re.sub(r"<\s*br\s*/?>", "\n", html)
    text = re.sub(r"</\s*(p|div|li|h[1-6])\s*>", "\n", text)
    text = re.sub(r"<[^>]+>", "", text)
    return unescape(text).strip()


def write_usage_doc(catalog_dir: str | Path, m: dict, v: dict) -> Path:
    """Write a readable usage-doc sidecar (.md) for one model version."""
    docs = Path(catalog_dir) / "usage"
    docs.mkdir(parents=True, exist_ok=True)
    safe = re.sub(r"[^\w.-]+", "_", f"{m['id']}_{m.get('name','')}")[:120]
    path = docs / f"{safe}.md"
    triggers = v.get("trainedWords") or []
    lines = [
        f"# {m.get('name')}  (model {m['id']})",
        "",
        f"- **Type:** {m.get('type')}   **NSFW:** {bool(m.get('nsfw'))}",
        f"- **Base model:** {v.get('baseModel')}   **Version:** {v.get('name')}",
        f"- **Creator:** {(m.get('creator') or {}).get('username')}",
        f"- **Page:** https://civitai.com/models/{m['id']}",
        f"- **Trigger words:** {', '.join(triggers) if triggers else '(none)'}",
        "",
        "## Author notes / recommended settings",
        "",
        _html_to_text(v.get("description")) or "(none provided)",
    ]
    path.write_text("\n".join(lines), encoding="utf-8")
    return path
