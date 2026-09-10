//! FreeBuff usage parsing (FreeBuff desktop app, freebuff.ai / CodeBuff).
//!
//! Each project keeps its own sqlite store:
//! `~/.config/freebuff-desktop/projects/<slug>/desktop-v2.db`
//!  - `threads` (id, project_path, title, model, created_at ms)
//!  - `messages` (thread_id, role, ts ms, metrics_json with
//!    `usage: {inputTokens, cachedInputTokens, outputTokens,
//!    reasoningOutputTokens?, totalTokens}` on assistant messages)
//! Each assistant message's `usage` is that turn's increment. Billing is
//! credit-based (`costUsd` is always 0), so cost is repriced from the local
//! sheet and unknown models stay at $0, flagged unpriced in the UI.

use std::collections::HashMap;
use std::path::PathBuf;

use rusqlite::Connection;

use crate::pricing;
use crate::sources::{home_dir, AGENT_FREEBUFF};

use super::UsageRecord;

pub struct Source {
    pub records: Vec<UsageRecord>,
}

fn projects_dir() -> Option<PathBuf> {
    home_dir().map(|h| {
        h.join(".config")
            .join("freebuff-desktop")
            .join("projects")
    })
}

pub fn scan(_cache: &mut crate::cache::FileCache, errors: &mut Vec<String>) -> Source {
    let Some(projects) = projects_dir() else {
        return Source { records: vec![] };
    };
    if !projects.is_dir() {
        return Source { records: vec![] };
    }

    let mut dbs: Vec<PathBuf> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&projects) {
        for entry in rd.flatten() {
            let db = entry.path().join("desktop-v2.db");
            if db.is_file() {
                dbs.push(db);
            }
        }
    }
    dbs.sort();

    if dbs.is_empty() {
        errors.push("FreeBuff: no session data found".into());
        return Source { records: vec![] };
    }

    let mut all = Vec::new();
    for db in dbs {
        match read_db(&db) {
            Ok(records) => all.extend(records),
            Err(e) => errors.push(format!("FreeBuff: cannot read {}: {}", db.display(), e)),
        }
    }
    Source { records: all }
}

struct ThreadMeta {
    model: String,
    title: String,
    cwd: String,
}

fn read_db(path: &PathBuf) -> Result<Vec<UsageRecord>, String> {
    let conn = Connection::open(path).map_err(|e| e.to_string())?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| e.to_string())?;
    let db_str = path.to_string_lossy().to_string();

    let mut threads: HashMap<String, ThreadMeta> = HashMap::new();
    {
        let mut stmt = conn
            .prepare("SELECT id, project_path, title, model FROM threads")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            let Ok((id, project_path, title, model)) = row else { continue };
            threads.insert(
                id,
                ThreadMeta {
                    model: model.filter(|m| !m.is_empty()).unwrap_or_else(|| "unknown".into()),
                    title: title.unwrap_or_default(),
                    cwd: project_path.unwrap_or_default(),
                },
            );
        }
    }

    if threads.is_empty() {
        return Ok(vec![]);
    }

    let mut records = Vec::new();
    {
        let mut stmt = conn
            .prepare("SELECT thread_id, metrics_json, ts FROM messages WHERE role = 'assistant'")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            let Ok((thread_id, metrics, ts_ms)) = row else { continue };
            let Some(meta) = threads.get(&thread_id) else { continue };
            let usage = metrics
                .as_deref()
                .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
                .and_then(|m| m.get("usage").cloned())
                .unwrap_or(serde_json::Value::Null);
            let num = |k: &str| usage.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
            let input_total = num("inputTokens");
            let output = num("outputTokens");
            let cached = num("cachedInputTokens");
            if input_total == 0 && output == 0 && cached == 0 {
                continue;
            }
            // `inputTokens` includes cached tokens; subtract so cached tokens
            // only bill at the discounted cache-read rate.
            let input = input_total.saturating_sub(cached);
            let cost = pricing::cost(&meta.model, input, output, 0, cached);
            // Prefer a billed cost if the app ever reports one.
            let cost = usage
                .get("costUsd")
                .and_then(|v| v.as_f64())
                .filter(|c| *c > 0.0)
                .unwrap_or(cost);
            records.push(UsageRecord {
                agent: AGENT_FREEBUFF,
                model: meta.model.clone(),
                ts: ts_ms.unwrap_or(0) / 1000,
                input,
                output,
                cache_creation: 0,
                cache_read: cached,
                cost,
                session_id: thread_id.clone(),
                title: meta.title.clone(),
                cwd: meta.cwd.clone(),
                path: db_str.clone(),
            });
        }
    }
    records.sort_by(|a, b| a.ts.cmp(&b.ts));
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_assistant_usage() {
        let dir = std::env::temp_dir().join(format!("tt-fb-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("desktop-v2.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE threads (id TEXT PRIMARY KEY, project_id TEXT NOT NULL, project_path TEXT NOT NULL, title TEXT NOT NULL DEFAULT 'New thread', status TEXT NOT NULL DEFAULT 'open', model TEXT);
                 CREATE TABLE messages (seq INTEGER PRIMARY KEY AUTOINCREMENT, thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE, request_id TEXT, input_id TEXT, role TEXT NOT NULL, parts_json TEXT NOT NULL DEFAULT '[]', attachments_json TEXT NOT NULL DEFAULT '[]', metrics_json TEXT NOT NULL DEFAULT '{}', ts INTEGER NOT NULL);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO threads (id, project_id, project_path, title, model) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params!["t1", "I:\\proj", "I:\\proj", "Fix bug", "gpt-5.1-codex"],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO messages (thread_id, role, metrics_json, ts) VALUES (?1, 'user', '{}', ?2)",
                rusqlite::params!["t1", 1788766218165i64],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO messages (thread_id, role, metrics_json, ts) VALUES (?1, 'assistant', ?2, ?3)",
                rusqlite::params![
                    "t1",
                    r#"{"usage":{"inputTokens":10123,"cachedInputTokens":256,"outputTokens":247,"totalTokens":10370},"costUsd":0}"#,
                    1788766334698i64
                ],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO messages (thread_id, role, metrics_json, ts) VALUES (?1, 'assistant', '{}', ?2)",
                rusqlite::params!["t1", 1788766335000i64],
            )
            .unwrap();
        }
        let records = read_db(&db_path).unwrap();
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!(r.model, "gpt-5.1-codex");
        assert_eq!(r.input, 10123 - 256);
        assert_eq!(r.output, 247);
        assert_eq!(r.cache_read, 256);
        assert_eq!(r.title, "Fix bug");
        assert_eq!(r.cwd, r#"I:\proj"#);
        assert_eq!(r.ts, 1788766334);
        assert!(r.cost > 0.0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
