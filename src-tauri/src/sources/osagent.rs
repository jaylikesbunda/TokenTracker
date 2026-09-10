//! OSAgent usage parsing (https://github.com/jaylikesbunda/OSAgent).
//!
//! OSAgent keeps everything in `~/.osagent/osagent.db`:
//!  - `sessions` (id, created_at secs, model, provider, metadata JSON with
//!    `name` title + `workspace_id` cwd)
//!  - `session_transcript` (session_id, seq, body JSON with role, timestamp
//!    RFC3339, and `tokens: {input, output, total, cached_read,
//!    cached_write, reasoning}` on assistant messages)
//! Each transcript row with tokens becomes one usage record; the model comes
//! from the parent session row and cost is repriced from the local sheet
//! (free-tier models resolve to $0 and stay zero).

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::DateTime;
use rusqlite::Connection;

use crate::pricing;
use crate::sources::{home_dir, AGENT_OSAGENT};

use super::UsageRecord;

pub struct Source {
    pub records: Vec<UsageRecord>,
}

fn db_path() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".osagent").join("osagent.db"))
}

pub fn scan(_cache: &mut crate::cache::FileCache, errors: &mut Vec<String>) -> Source {
    let Some(db) = db_path() else {
        return Source { records: vec![] };
    };
    if !db.is_file() {
        return Source { records: vec![] };
    }
    match read_db(&db) {
        Ok(records) => Source { records },
        Err(e) => {
            errors.push(format!("OSAgent: {}", e));
            Source { records: vec![] }
        }
    }
}

struct SessionMeta {
    model: String,
    title: String,
    cwd: String,
    created_at: i64,
}

fn parse_ts(s: &str) -> i64 {
    DateTime::parse_from_rfc3339(s).map(|d| d.timestamp()).unwrap_or(0)
}

fn read_db(path: &PathBuf) -> Result<Vec<UsageRecord>, String> {
    let conn = Connection::open(path).map_err(|e| e.to_string())?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| e.to_string())?;
    let db_str = path.to_string_lossy().to_string();

    // Session metadata first.
    let mut sessions: HashMap<String, SessionMeta> = HashMap::new();
    {
        let mut stmt = conn
            .prepare("SELECT id, created_at, model, metadata FROM sessions")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<Vec<u8>>>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            let Ok((id, created_at, model, metadata)) = row else { continue };
            let (title, cwd) = metadata
                .as_deref()
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
                .map(|m| {
                    (
                        m.get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        m.get("workspace_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string(),
                    )
                })
                .unwrap_or_default();
            sessions.insert(
                id,
                SessionMeta {
                    model: model.unwrap_or_else(|| "unknown".into()),
                    title,
                    cwd,
                    created_at: created_at.unwrap_or(0),
                },
            );
        }
    }

    if sessions.is_empty() {
        return Ok(vec![]);
    }

    let mut records = Vec::new();
    {
        let mut stmt = conn
            .prepare("SELECT session_id, body FROM session_transcript")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)))
            .map_err(|e| e.to_string())?;
        for row in rows {
            let Ok((session_id, body)) = row else { continue };
            let Some(meta) = sessions.get(&session_id) else { continue };
            let Ok(v) = serde_json::from_slice::<serde_json::Value>(&body) else { continue };
            let tokens = match v.get("tokens") {
                Some(t) if !t.is_null() => t,
                _ => continue,
            };
            let num = |k: &str| tokens.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
            let input_total = num("input");
            let output = num("output");
            let cached_read = num("cached_read");
            let cached_write = num("cached_write");
            if input_total == 0 && output == 0 && cached_read == 0 && cached_write == 0 {
                continue;
            }
            // `input` includes cached tokens; subtract so cached tokens only
            // bill at the discounted cache-read rate.
            let input = input_total.saturating_sub(cached_read.saturating_add(cached_write));
            let model = if meta.model.is_empty() { "unknown".to_string() } else { meta.model.clone() };
            let cost = pricing::cost(&model, input, output, cached_write, cached_read);
            let ts = v
                .get("timestamp")
                .and_then(|t| t.as_str())
                .map(parse_ts)
                .unwrap_or(meta.created_at);
            // Skip user messages that accidentally carry tokens:null (already
            // filtered) — only assistant-sized rows matter, but role is not
            // enforced so reasoning/tool rows still count.
            records.push(UsageRecord {
                agent: AGENT_OSAGENT,
                model,
                ts,
                input,
                output,
                cache_creation: cached_write,
                cache_read: cached_read,
                cost,
                session_id: session_id.clone(),
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
    fn parses_transcript_tokens() {
        let dir = std::env::temp_dir().join(format!("tt-osagent-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("osagent.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE sessions (id TEXT PRIMARY KEY, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, model TEXT NOT NULL, provider TEXT NOT NULL, messages BLOB NOT NULL, metadata BLOB, parent_id TEXT, agent_type TEXT NOT NULL DEFAULT 'primary', task_status TEXT NOT NULL DEFAULT 'active', context_state BLOB);
                 CREATE TABLE session_transcript (session_id TEXT NOT NULL, seq INTEGER NOT NULL, body BLOB NOT NULL, PRIMARY KEY (session_id, seq));",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO sessions (id, created_at, updated_at, model, provider, messages, metadata) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    "sess-1",
                    1788772135i64,
                    1788772159i64,
                    "gpt-5.1-codex",
                    "openai",
                    b"[]".to_vec(),
                    br#"{"name":"Greeting","workspace_id":"GhostESP"}"#.to_vec(),
                ],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO session_transcript (session_id, seq, body) VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    "sess-1",
                    0,
                    br#"{"role":"user","content":"hey","timestamp":"2026-09-07T09:09:00.312Z","tokens":null}"#.to_vec(),
                ],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO session_transcript (session_id, seq, body) VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    "sess-1",
                    1,
                    br#"{"role":"assistant","content":"hi","timestamp":"2026-09-07T09:09:00.625Z","tokens":{"input":1000,"output":44,"total":1044,"cached_read":200,"cached_write":0,"reasoning":null}}"#.to_vec(),
                ],
            )
            .unwrap();
        }
        let records = read_db(&db_path).unwrap();
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!(r.model, "gpt-5.1-codex");
        assert_eq!(r.input, 800);
        assert_eq!(r.output, 44);
        assert_eq!(r.cache_read, 200);
        assert_eq!(r.title, "Greeting");
        assert_eq!(r.cwd, "GhostESP");
        assert!(r.cost > 0.0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
