//! Lore subsystem (Phase 4). Synthetrix treats the IP's lore-bible git repo as
//! the on-disk source of truth and indexes its markdown into the per-IP
//! `project.sqlite` (`lore_index` table). The **reader** browses that index and
//! reads any entry's full text on demand; the **manager** guardrails (positive
//! anchoring / vocabulary tiebreaker) surface canonical terms per entry so the
//! forge and prompt matrix speak the IP's own vocabulary rather than drifting.

use rusqlite::Connection;
use std::path::{Path, PathBuf};

/// One indexed lore document.
#[derive(Clone, Default)]
pub struct LoreEntry {
    pub id: i64,
    /// Top-level lore folder: characters / factions / vehicles / weapons /
    /// world / concepts / timeline / … ("root" for repo-root docs).
    pub kind: String,
    /// Entity name (folder for profile.md/README.md, else the file stem).
    pub name: String,
    pub rel_path: String,
    pub title: String,
    pub summary: String,
    /// Canonical terms lifted from the doc (bold spans + title), comma-joined —
    /// the vocabulary tiebreaker's raw material.
    pub vocab: String,
    pub updated_at: String,
}

/// Directories never worth indexing (VCS, tool state, engine/media binaries).
fn skip_dir(name: &str) -> bool {
    name.starts_with('.')
        || matches!(
            name,
            "node_modules" | "target" | "media" | "artifacts" | "batches" | "TEST" | "__pycache__"
        )
}

/// First-path-component bucket, or "root" for repo-root files.
fn classify_kind(rel: &Path) -> String {
    let mut comps = rel.components();
    match (comps.next(), comps.clone().next()) {
        // has at least one dir before the file → that dir is the kind
        (Some(first), Some(_)) => first.as_os_str().to_string_lossy().to_ascii_lowercase(),
        _ => "root".to_string(),
    }
}

/// Human name: parent folder for profile.md / README.md, else the file stem.
fn derive_name(rel: &Path) -> String {
    let stem = rel
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    if stem.eq_ignore_ascii_case("profile") || stem.eq_ignore_ascii_case("readme") {
        if let Some(parent) = rel
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
        {
            if !parent.is_empty() {
                return parent.to_string();
            }
        }
    }
    stem
}

/// Pull (title, summary, vocab) out of a markdown body.
fn extract(text: &str, fallback_name: &str) -> (String, String, String) {
    let mut title = String::new();
    let mut summary = String::new();
    let mut vocab: Vec<String> = Vec::new();
    let mut in_fence = false;

    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        // first H1 is the title
        if title.is_empty() {
            if let Some(h) = t.strip_prefix("# ") {
                title = h.trim().to_string();
                continue;
            }
        }
        // collect bold spans as canonical vocab
        for term in bold_spans(t) {
            let clean = term.trim().trim_end_matches(':').trim().to_string();
            if clean.len() >= 3
                && clean.len() <= 40
                && !vocab.iter().any(|v| v.eq_ignore_ascii_case(&clean))
            {
                vocab.push(clean);
            }
        }
        // first prose paragraph → summary (skip headings, tables, lists,
        // blockquotes, and **Key:** metadata lines)
        if summary.is_empty()
            && !t.is_empty()
            && !t.starts_with('#')
            && !t.starts_with('|')
            && !t.starts_with('-')
            && !t.starts_with('*')
            && !t.starts_with('>')
            && !t.starts_with("**")
            && !t.starts_with("---")
        {
            summary = t.to_string();
        }
    }

    if title.is_empty() {
        title = fallback_name.to_string();
    }
    if summary.len() > 240 {
        let cut = summary
            .char_indices()
            .take_while(|(i, _)| *i < 237)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        summary = format!("{}…", &summary[..cut]);
    }
    vocab.truncate(14);
    (title, summary, vocab.join(", "))
}

/// Extract the inner text of every `**bold**` span on a line.
fn bold_spans(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(a) = rest.find("**") {
        let after = &rest[a + 2..];
        if let Some(b) = after.find("**") {
            let inner = &after[..b];
            if !inner.is_empty() && !inner.contains('*') {
                out.push(inner.to_string());
            }
            rest = &after[b + 2..];
        } else {
            break;
        }
    }
    out
}

/// Walk `lore_root` for markdown and build lore entries (unsorted, id==0).
pub fn scan(lore_root: &Path) -> Vec<LoreEntry> {
    let mut out = Vec::new();
    if !lore_root.is_dir() {
        return out;
    }
    let mut stack = vec![lore_root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                let dn = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !skip_dir(dn) {
                    stack.push(p);
                }
                continue;
            }
            let is_md = p
                .extension()
                .and_then(|x| x.to_str())
                .map(|x| x.eq_ignore_ascii_case("md"))
                .unwrap_or(false);
            if !is_md {
                continue;
            }
            let rel = p.strip_prefix(lore_root).unwrap_or(&p).to_path_buf();
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            let kind = classify_kind(&rel);
            let name = derive_name(&rel);
            let text = std::fs::read_to_string(&p).unwrap_or_default();
            let (title, summary, vocab) = extract(&text, &name);
            out.push(LoreEntry {
                id: 0,
                kind,
                name,
                rel_path: rel_str,
                title,
                summary,
                vocab,
                updated_at: String::new(),
            });
        }
    }
    out.sort_by(|a, b| a.kind.cmp(&b.kind).then(a.name.cmp(&b.name)));
    out
}

/// Rebuild the `lore_index` table from a fresh scan; returns the row count.
pub fn reindex(conn: &Connection, lore_root: &Path) -> usize {
    let entries = scan(lore_root);
    let _ = conn.execute("DELETE FROM lore_index", []);
    let _ = conn.execute_batch("BEGIN");
    for e in &entries {
        let _ = conn.execute(
            "INSERT INTO lore_index(kind,name,rel_path,title,summary,vocab)
             VALUES(?1,?2,?3,?4,?5,?6)",
            rusqlite::params![e.kind, e.name, e.rel_path, e.title, e.summary, e.vocab],
        );
    }
    let _ = conn.execute_batch("COMMIT");
    entries.len()
}

const LORE_COLS: &str = "id,kind,name,rel_path,title,summary,vocab,updated_at";

fn row_to_entry(r: &rusqlite::Row) -> rusqlite::Result<LoreEntry> {
    Ok(LoreEntry {
        id: r.get(0)?,
        kind: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
        name: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
        rel_path: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
        title: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
        summary: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
        vocab: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
        updated_at: r.get::<_, Option<String>>(7)?.unwrap_or_default(),
    })
}

/// Count indexed rows (used to decide whether a first-open auto-reindex is due).
pub fn count(conn: &Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM lore_index", [], |r| r.get(0))
        .unwrap_or(0)
}

/// Filtered browse: `kind` exact-matches the bucket, `search` matches name/
/// title/vocab/summary substrings.
pub fn query(
    conn: &Connection,
    kind: Option<&str>,
    search: Option<&str>,
    limit: i64,
) -> Vec<LoreEntry> {
    let mut sql = format!("SELECT {LORE_COLS} FROM lore_index WHERE 1=1");
    if let Some(k) = kind {
        sql.push_str(&format!(" AND kind='{}'", k.replace('\'', "''")));
    }
    if let Some(s) = search {
        let s = s.replace('\'', "''");
        sql.push_str(&format!(
            " AND (name LIKE '%{s}%' OR title LIKE '%{s}%' OR vocab LIKE '%{s}%' OR summary LIKE '%{s}%')"
        ));
    }
    sql.push_str(&format!(" ORDER BY kind, name LIMIT {limit}"));
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map([], row_to_entry)
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

/// Distinct kinds present in the index, for the tab's filter chips.
pub fn kinds(conn: &Connection) -> Vec<String> {
    let mut stmt = match conn.prepare("SELECT DISTINCT kind FROM lore_index ORDER BY kind") {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map([], |r| r.get::<_, String>(0))
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

pub fn entry_by_id(conn: &Connection, id: i64) -> Option<LoreEntry> {
    conn.query_row(
        &format!("SELECT {LORE_COLS} FROM lore_index WHERE id=?1"),
        [id],
        row_to_entry,
    )
    .ok()
}

/// Resolve an entry's on-disk absolute path under the lore root.
pub fn abs_path(lore_root: &str, rel_path: &str) -> PathBuf {
    Path::new(lore_root).join(rel_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_pulls_title_summary_vocab() {
        let md = "# Protagonist\n\n**Status:** In Development\n\nThe protagonist is a \
                  corporate combat operative deployed from **The Inexorable** via drop pod.\n\n\
                  ## Background\n";
        let (title, summary, vocab) = extract(md, "protagonist");
        assert_eq!(title, "Protagonist");
        assert!(summary.starts_with("The protagonist is a corporate"));
        assert!(vocab.contains("The Inexorable"));
        // the **Status:** line is metadata, not the summary paragraph
        assert!(!summary.contains("In Development"));
    }

    #[test]
    fn classify_and_name() {
        let rel = Path::new("characters/corporate/protagonist/profile.md");
        assert_eq!(classify_kind(rel), "characters");
        assert_eq!(derive_name(rel), "protagonist");
        let root = Path::new("LORE_PRIMER.md");
        assert_eq!(classify_kind(root), "root");
        assert_eq!(derive_name(root), "LORE_PRIMER");
    }
}
