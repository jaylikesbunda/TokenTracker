//! CommandCode CLI usage parsing (`command-code` / `cmd`, commandcode.ai).
//!
//! Sessions live under `~/.commandcode/projects/<slug>/<session>.jsonl`
//! with a sibling `<session>.meta.json` carrying the title:
//!  - `{"type":"session",...,"cwd":"...","timestamp":"..."}`
//!  - `{"type":"model_change",...,"model":"..."}`
//!  - `{"type":"message",...,"timestamp":"...","message":{"role":..},
//!     "usage":{"inputTokens":n,"outputTokens":n,"cacheReadTokens":n,
//!              "cacheWriteTokens":n,"costUsd":f},"model":"..."}`
//! `*.checkpoints.jsonl` files hold turn prompts, not usage, and are skipped.
//! `costUsd` is the billed cost, so it is used verbatim when present and the
//! local price sheet is only a fallback.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use chrono::DateTime;

use crate::cache::FileCache;
use crate::pricing;
use crate::sources::{home_dir, AGENT_COMMANDCODE};

use super::UsageRecord;

pub struct Source {
    pub records: Vec<UsageRecord>,
}

fn commandcode_dir() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".commandcode"))
}

pub fn scan(cache: &mut FileCache, errors: &mut Vec<String>) -> Source {
    let Some(base) = commandcode_dir() else {
        return Source { records: vec![] };
    };
    let projects = base.join("projects");
    if !projects.is_dir() {
        return Source { records: vec![] };
    }

    let mut files: Vec<PathBuf> = Vec::new();
    collect_sessions(&projects, &mut files);
    files.sort();

    if files.is_empty() {
        errors.push("CommandCode: no session data found".into());
        return Source { records: vec![] };
    }

    let mut all: Vec<UsageRecord> = Vec::new();
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
            Err(e) => errors.push(format!("CommandCode: cannot read {}: {}", path.display(), e)),
        }
    }

    Source { records: all }
}

fn collect_sessions(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_sessions(&path, out);
        } else if path.extension().map(|e| e == "jsonl").unwrap_or(false) {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.ends_with(".checkpoints.jsonl") {
                    continue;
                }
            }
            out.push(path);
        }
    }
}

fn parse_ts_str(s: &str) -> i64 {
    DateTime::parse_from_rfc3339(s).map(|d| d.timestamp()).unwrap_or(0)
}

fn meta_title(path: &Path) -> (String, String) {
    let meta = path.with_extension("meta.json");
    let Ok(raw) = std::fs::read_to_string(&meta) else {
        return (String::new(), String::new());
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return (String::new(), String::new());
    };
    (
        v.get("title").and_then(|t| t.as_str()).unwrap_or_default().to_string(),
        v.get("model").and_then(|t| t.as_str()).unwrap_or_default().to_string(),
    )
}

fn parse_session_file(path: &Path) -> Vec<UsageRecord> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return vec![],
    };
    let session_id = path
        .file_stem()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let path_str = path.to_string_lossy().to_string();
    let (meta_title, meta_model) = meta_title(path);

    let mut cwd = String::new();
    let mut session_ts = 0i64;
    let mut current_model = meta_model.clone();
    // Buffer messages that arrive before the session line (shouldn't happen,
    // but the session line is normally first).
    let mut records: Vec<UsageRecord> = Vec::new();

    for line in BufReader::new(file).lines().flatten() {
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        let kind = json.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match kind {
            "session" => {
                if let Some(c) = json.get("cwd").and_then(|v| v.as_str()) {
                    cwd = c.to_string();
                }
                if let Some(t) = json.get("timestamp").and_then(|v| v.as_str()) {
                    session_ts = parse_ts_str(t);
                }
            }
            "model_change" => {
                if let Some(m) = json.get("model").and_then(|v| v.as_str()) {
                    if !m.is_empty() {
                        current_model = m.to_string();
                    }
                }
            }
            "message" => {
                let Some(u) = json.get("usage") else { continue };
                let num = |k: &str| u.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
                let input = num("inputTokens");
                let output = num("outputTokens");
                let cache_read = num("cacheReadTokens");
                let cache_write = num("cacheWriteTokens");
                if input == 0 && output == 0 && cache_read == 0 && cache_write == 0 {
                    continue;
                }
                let model = json
                    .get("model")
                    .and_then(|v| v.as_str())
                    .filter(|m| !m.is_empty())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| {
                        if current_model.is_empty() {
                            "unknown".to_string()
                        } else {
                            current_model.clone()
                        }
                    });
                // Track model timeline for later messages without an explicit model.
                if json.get("model").and_then(|v| v.as_str()).is_some() {
                    current_model = model.clone();
                }
                // Prefer the billed cost; fall back to the local price sheet.
                let cost = u
                    .get("costUsd")
                    .and_then(|v| v.as_f64())
                    .unwrap_or_else(|| pricing::cost(&model, input, output, cache_write, cache_read));
                let ts = json
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .map(parse_ts_str)
                    .unwrap_or(session_ts);
                records.push(UsageRecord {
                    agent: AGENT_COMMANDCODE,
                    model,
                    ts,
                    input,
                    output,
                    cache_creation: cache_write,
                    cache_read,
                    cost,
                    session_id: session_id.clone(),
                    title: meta_title.clone(),
                    cwd: cwd.clone(),
                    path: path_str.clone(),
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

    fn write_session(dir: &Path, name: &str, lines: &[&str]) -> PathBuf {
        let file = dir.join(name);
        let mut fh = File::create(&file).unwrap();
        for l in lines {
            writeln!(fh, "{}", l).unwrap();
        }
        file
    }

    #[test]
    fn parses_usage_with_billed_cost() {
        let dir = std::env::temp_dir().join(format!("tt-cc-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = write_session(
            &dir,
            "sess-1.jsonl",
            &[
                r#"{"type":"session","version":3,"id":"sess-1","timestamp":"2026-09-03T08:53:47.837Z","cwd":"I:\\proj"}"#,
                r#"{"type":"model_change","id":"a87","parentId":null,"timestamp":"2026-09-03T08:56:37.287Z","model":"deepseek/deepseek-v4-flash"}"#,
                r#"{"type":"message","id":"m1","parentId":"a87","timestamp":"2026-09-03T08:57:45.805Z","message":{"role":"assistant","content":[]},"usage":{"inputTokens":19883,"outputTokens":735,"cacheReadTokens":19840,"cacheWriteTokens":0,"costUsd":0.00499824},"model":"deepseek/deepseek-v4-flash","effort":"high"}"#,
                r#"{"type":"message","id":"m2","timestamp":"2026-09-03T08:58:00.000Z","message":{"role":"user","content":[]}}"#,
            ],
        );
        std::fs::write(dir.join("sess-1.meta.json"), r#"{"model":"deepseek/deepseek-v4-flash","title":"Compare Display"}"#).unwrap();

        let records = parse_session_file(&file);
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!(r.model, "deepseek/deepseek-v4-flash");
        assert_eq!(r.input, 19883);
        assert_eq!(r.output, 735);
        assert_eq!(r.cache_read, 19840);
        assert!((r.cost - 0.00499824).abs() < 1e-9);
        assert_eq!(r.title, "Compare Display");
        assert_eq!(r.cwd, r#"I:\proj"#);
        assert_eq!(r.session_id, "sess-1");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn falls_back_to_pricing_without_cost() {
        let dir = std::env::temp_dir().join(format!("tt-cc-test2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = write_session(
            &dir,
            "sess-2.jsonl",
            &[
                r#"{"type":"session","id":"sess-2","timestamp":"2026-09-03T08:53:47.837Z","cwd":"/tmp"}"#,
                r#"{"type":"message","id":"m1","timestamp":"2026-09-03T08:57:45.805Z","message":{"role":"assistant","content":[]},"usage":{"inputTokens":100,"outputTokens":50},"model":"gpt-5.1-codex"}"#,
            ],
        );
        let records = parse_session_file(&file);
        assert_eq!(records.len(), 1);
        assert!(records[0].cost > 0.0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
