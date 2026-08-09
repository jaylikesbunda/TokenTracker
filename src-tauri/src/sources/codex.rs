//! Codex CLI usage parsing.
//!
//! Two storage generations are supported:
//!  - Legacy: `~/.codex/sessions/**/*.jsonl` (streamed event logs)
//!  - New:    `~/.codex/state_*.sqlite` `threads` table

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use chrono::DateTime;
use rusqlite::Connection;

use crate::cache::FileCache;
use crate::pricing;
use crate::sources::{home_dir, AGENT_CODEX};

use super::UsageRecord;

pub struct Source {
    pub records: Vec<UsageRecord>,
}

fn codex_dir() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".codex"))
}

pub fn scan(cache: &mut FileCache, errors: &mut Vec<String>) -> Source {
    let Some(codex) = codex_dir() else {
        return Source { records: vec![] };
    };
    if !codex.is_dir() {
        return Source { records: vec![] };
    }

    let mut all: Vec<UsageRecord> = Vec::new();

    let sessions_dir = codex.join("sessions");
    let mut any_legacy = false;
    if sessions_dir.is_dir() {
        let mut files: Vec<PathBuf> = Vec::new();
        collect_jsonl(&sessions_dir, &mut files);
        files.sort();
        any_legacy = !files.is_empty();
        for path in files {
            match File::open(&path) {
                Ok(f) => {
                    let (mtime, size) = f
                        .metadata()
                        .map(|m| (m.modified().unwrap_or(UNIX_EPOCH), m.len()))
                        .unwrap_or((UNIX_EPOCH, 0));
                    let records = match cache.get(&path, mtime, size) {
                        Some(r) => r,
                        None => {
                            let records = parse_session_file(&path);
                            cache.insert(path, mtime, size, records.clone());
                            records
                        }
                    };
                    all.extend(records);
                }
                Err(e) => errors.push(format!("Codex CLI: cannot read {}: {}", path.display(), e)),
            }
        }
    }

    let mut any_sqlite = false;
    if let Ok(rd) = std::fs::read_dir(&codex) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "sqlite").unwrap_or(false)
                && path.file_name().map(|n| n.to_string_lossy().starts_with("state_")).unwrap_or(false)
            {
                any_sqlite = true;
                match read_threads_db(&path) {
                    Ok(records) => all.extend(records),
                    Err(e) => errors.push(format!("Codex CLI: cannot read {}: {}", path.display(), e)),
                }
            }
        }
    }

    if all.is_empty() && !any_legacy && !any_sqlite {
        errors.push("Codex CLI: no session data found".into());
    }

    Source { records: all }
}

fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl(&path, out);
        } else if path.extension().map(|e| e == "jsonl").unwrap_or(false) {
            out.push(path);
        }
    }
}

/// Parse both the older `response_item`/`event_msg` line shapes that Codex
/// CLI has produced over time.
fn parse_session_file(path: &Path) -> Vec<UsageRecord> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return vec![],
    };
    let session_id = path
        .file_stem()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    let mut records: Vec<UsageRecord> = Vec::new();
    for line in BufReader::new(file).lines().flatten() {
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) else { continue };

        let payload = json.get("payload");
        let model = payload
            .and_then(|p| p.get("model"))
            .or_else(|| json.get("model"))
            .and_then(|m| m.as_str())
            .unwrap_or("unknown")
            .to_string();

        let usage = payload
            .and_then(|p| p.get("usage"))
            .or_else(|| payload.and_then(|p| p.get("tokens")))
            .or_else(|| json.get("usage"));

        let num = |u: &serde_json::Value, k: &str| u.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
        let Some(usage) = usage else { continue };
        let input = num(usage, "input_tokens");
        let output = num(usage, "output_tokens");
        if input == 0 && output == 0 {
            continue;
        }

        let timestamp = json.get("timestamp").and_then(|t| t.as_str());
        let ts = timestamp
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|d| d.timestamp())
            .unwrap_or_else(|| {
                json.get("timestamp")
                    .and_then(|t| t.as_u64())
                    .map(|ms| (ms / 1000) as i64)
                    .unwrap_or(0)
            });

        let cost = pricing::cost(&model, input, output, 0, 0);

        records.push(UsageRecord {
            agent: AGENT_CODEX,
            model,
            ts,
            input,
            output,
            cache_creation: 0,
            cache_read: 0,
            cost,
            session_id: session_id.clone(),
            title: String::new(),
            cwd: path
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default(),
            path: path.to_string_lossy().to_string(),
        });
    }
    records
}

/// New Codex storage: the `threads` table (tokens_used may be a JSON object
/// `{"input": n, "output": n}` or an integer total).
fn read_threads_db(path: &Path) -> Result<Vec<UsageRecord>, String> {
    let conn = Connection::open(path).map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT id, title, cwd, model, tokens_used, created_at_ms FROM threads ORDER BY created_at_ms DESC",
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<i64>>(5)?,
            ))
        })
        .map_err(|e| e.to_string())?;

    let mut records = Vec::new();
    for row in rows {
        let Ok((id, title, cwd, model, tokens_used, created_at_ms)) = row else { continue };
        let model = model.unwrap_or_else(|| "unknown".into());
        let (input, output) = parse_tokens(tokens_used.as_deref());
        let cost = pricing::cost(&model, input, output, 0, 0);
        records.push(UsageRecord {
            agent: AGENT_CODEX,
            model,
            ts: created_at_ms.unwrap_or(0) / 1000,
            input,
            output,
            cache_creation: 0,
            cache_read: 0,
            cost,
            session_id: id,
            title: title.unwrap_or_default(),
            cwd: cwd.unwrap_or_default(),
            path: path.to_string_lossy().to_string(),
        });
    }
    Ok(records)
}

fn parse_tokens(raw: Option<&str>) -> (u64, u64) {
    let Some(raw) = raw else { return (0, 0) };
    let raw = raw.trim();
    if raw.is_empty() {
        return (0, 0);
    }
    if raw.starts_with('{') {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) {
            let input = v.get("input").and_then(|x| x.as_u64()).unwrap_or(0);
            let output = v.get("output").and_then(|x| x.as_u64()).unwrap_or(0);
            return (input, output);
        }
        return (0, 0);
    }
    if let Ok(n) = raw.parse::<u64>() {
        return (n, 0);
    }
    (0, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parses_legacy_event_msg() {
        let dir = std::env::temp_dir().join(format!("tt-codex-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("sess1.jsonl");
        let mut fh = File::create(&file).unwrap();
        writeln!(
            fh,
            r#"{{"timestamp":"2026-08-09T04:20:23.476Z","type":"event_msg","payload":{{"type":"agent_message","model":"gpt-5.1-codex","usage":{{"input_tokens":100,"output_tokens":50}}}}}}"#
        )
        .unwrap();
        writeln!(fh, r#"{{"type":"event_msg","payload":{{"type":"agent_message","content":"no usage"}}}}"#)
            .unwrap();
        drop(fh);

        let records = parse_session_file(&file);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].model, "gpt-5.1-codex");
        assert_eq!(records[0].input, 100);
        assert_eq!(records[0].output, 50);
        assert!(records[0].cost > 0.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_newer_tokens_shape() {
        let dir = std::env::temp_dir().join(format!("tt-codex-test2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("sess2.jsonl");
        let mut fh = File::create(&file).unwrap();
        writeln!(
            fh,
            r#"{{"timestamp":"2026-08-09T04:20:23.476Z","type":"event_msg","payload":{{"type":"agent_message","tokens":{{"input_tokens":10,"output_tokens":20}}}}}}"#
        )
        .unwrap();
        drop(fh);
        let records = parse_session_file(&file);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].input, 10);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn token_json_parsing() {
        assert_eq!(parse_tokens(Some(r#"{"input": 5, "output": 7}"#)), (5, 7));
        assert_eq!(parse_tokens(Some("123")), (123, 0));
        assert_eq!(parse_tokens(Some("")), (0, 0));
        assert_eq!(parse_tokens(None), (0, 0));
    }
}


