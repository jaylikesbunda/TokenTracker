//! Claude Code usage parsing from `~/.claude/projects/<encoded>/<session>.jsonl`.
//!
//! Assistant lines carry `message.usage` (input/output/cache tokens) and
//! `message.model`. Titles come from `custom-title` / `ai-title` lines.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use chrono::DateTime;

use crate::cache::FileCache;
use crate::pricing;
use crate::sources::{home_dir, AGENT_CLAUDE};

use super::UsageRecord;

pub fn data_dir() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".claude").join("projects"))
}

pub struct Source {
    pub records: Vec<UsageRecord>,
}

pub fn scan(cache: &mut FileCache, errors: &mut Vec<String>) -> Source {
    let Some(projects) = data_dir() else {
        return Source { records: vec![] };
    };
    if !projects.is_dir() {
        return Source { records: vec![] };
    }

    let mut files: Vec<PathBuf> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&projects) {
        for project in rd.flatten() {
            if !project.path().is_dir() {
                continue;
            }
            if let Ok(sessions) = std::fs::read_dir(project.path()) {
                for session in sessions.flatten() {
                    let path = session.path();
                    if path.extension().map(|e| e == "jsonl").unwrap_or(false) {
                        files.push(path);
                    }
                }
            }
        }
    }
    files.sort();

    let mut all: Vec<UsageRecord> = Vec::new();
    let mut any_failed = false;

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
                        let records = parse_session_file(&path, &mut any_failed);
                        cache.insert(path, mtime, size, records.clone());
                        records
                    }
                };
                all.extend(records);
            }
            Err(e) => {
                any_failed = true;
                errors.push(format!("Claude Code: cannot read {}: {}", path.display(), e));
            }
        }
    }

    if all.is_empty() && !any_failed {
        errors.push("Claude Code: no session data found".into());
    }

    Source { records: all }
}

fn parse_session_file(path: &Path, failed: &mut bool) -> Vec<UsageRecord> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => {
            *failed = true;
            return vec![];
        }
    };

    let project_dir = path
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    let session_id = path
        .file_stem()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    let mut title = String::new();
    let mut records: Vec<UsageRecord> = Vec::new();
    let mut any_title = false;

    for line in BufReader::new(file).lines().flatten() {
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        let Some(kind) = json.get("type").and_then(|t| t.as_str()) else { continue };

        match kind {
            "custom-title" => {
                if let Some(t) = json.get("customTitle").and_then(|t| t.as_str()) {
                    title = t.to_string();
                    any_title = true;
                }
            }
            "ai-title" => {
                if !any_title {
                    if let Some(t) = json.get("aiTitle").and_then(|t| t.as_str()) {
                        title = t.to_string();
                    }
                }
            }
            "assistant" => {
                let Some(message) = json.get("message") else { continue };
                let Some(usage) = message.get("usage") else { continue };
                let model = message
                    .get("model")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let Some(timestamp) = json.get("timestamp").and_then(|t| t.as_str()) else { continue };
                let Ok(ts) = DateTime::parse_from_rfc3339(timestamp) else { continue };
                let ts = ts.timestamp();

                let num = |k: &str| usage.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
                let input = num("input_tokens");
                let output = num("output_tokens");
                let cache_creation = num("cache_creation_input_tokens");
                let cache_read = num("cache_read_input_tokens");
                let cost = pricing::cost(&model, input, output, cache_creation, cache_read);

                records.push(UsageRecord {
                    agent: AGENT_CLAUDE,
                    model,
                    ts,
                    input,
                    output,
                    cache_creation,
                    cache_read,
                    cost,
                    session_id: session_id.clone(),
                    title: title.clone(),
                    cwd: project_dir.clone(),
                    path: path.to_string_lossy().to_string(),
                });
            }
            _ => {}
        }
    }

    records
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parses_assistant_usage_and_titles() {
        let dir = std::env::temp_dir().join(format!("tt-claude-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("I--GhostESP2-Ghost-ESP")).unwrap();
        let file = dir.join("I--GhostESP2-Ghost-ESP").join("abc123.jsonl");
        let mut fh = File::create(&file).unwrap();
        writeln!(fh, r#"{{"type":"custom-title","customTitle":"OTA fix","sessionId":"abc123"}}"#).unwrap();
        writeln!(
            fh,
            r#"{{"type":"assistant","timestamp":"2026-08-09T04:20:23.476Z","message":{{"model":"claude-opus-5","usage":{{"input_tokens":2,"output_tokens":204,"cache_creation_input_tokens":11694,"cache_read_input_tokens":25877}}}}}}"#
        )
        .unwrap();
        writeln!(fh, r#"{{"type":"user","message":{{"content":"hi"}}}}"#).unwrap();
        drop(fh);

        let mut failed = false;
        let records = parse_session_file(&file, &mut failed);
        assert!(!failed);
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!(r.model, "claude-opus-5");
        assert_eq!(r.input, 2);
        assert_eq!(r.output, 204);
        assert_eq!(r.cache_creation, 11694);
        assert_eq!(r.cache_read, 25877);
        assert!(r.cost > 0.0, "claude-opus-5 should have a price");
        assert_eq!(r.title, "OTA fix");
        assert_eq!(r.cwd, "I--GhostESP2-Ghost-ESP");
        assert_eq!(r.session_id, "abc123");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ignores_non_usage_lines() {
        let dir = std::env::temp_dir().join(format!("tt-claude-test2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("x.jsonl");
        let mut fh = File::create(&file).unwrap();
        writeln!(fh, "not json").unwrap();
        writeln!(fh, r#"{{"type":"user","message":{{}}}}"#).unwrap();
        writeln!(fh, r#"{{"type":"assistant","timestamp":"2026-08-09T04:20:23.476Z","message":{{"usage":{{"input_tokens":5}}}}}}"#).unwrap();
        drop(fh);

        let mut failed = false;
        let records = parse_session_file(&file, &mut failed);
        assert_eq!(records.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}


