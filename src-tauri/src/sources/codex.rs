//! Codex CLI usage parsing.
//!
//! Three storage generations are supported:
//!  - Legacy: `~/.codex/sessions/**/*.jsonl` `event_msg` / `token_count`
//!    shapes (`payload.info.last_token_usage`, per-response increments)
//!  - New:    `~/.codex/sessions/**/*.jsonl` `token_usage_record` shapes
//!    (`payload.usage`, per-response increments, model resolved via the
//!    surrounding `turn_context` entries)
//!  - Index:  `~/.codex/state_*.sqlite` `threads` table (fallback metadata
//!    + single-record fallback when a session has no jsonl usage)

use std::collections::HashMap;
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

/// Session-level metadata harvested from the `threads` index DBs so jsonl
/// records (which carry no title) can be enriched.
#[derive(Default, Clone)]
struct ThreadMeta {
    title: String,
    cwd: String,
    model: String,
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
    // Thread metadata (title/cwd/model) used to enrich jsonl records.
    let mut thread_meta: HashMap<String, ThreadMeta> = HashMap::new();
    // Fallback token records for sessions that have no jsonl usage at all.
    let mut db_fallback: Vec<UsageRecord> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&codex) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "sqlite").unwrap_or(false)
                && path.file_name().map(|n| n.to_string_lossy().starts_with("state_")).unwrap_or(false)
            {
                any_sqlite = true;
                match read_threads_db(&path) {
                    Ok((records, meta)) => {
                        for (id, m) in meta {
                            thread_meta.entry(id).or_insert(m);
                        }
                        db_fallback.extend(records);
                    }
                    Err(e) => errors.push(format!("Codex CLI: cannot read {}: {}", path.display(), e)),
                }
            }
        }
    }

    if all.is_empty() && !any_legacy && !any_sqlite {
        errors.push("Codex CLI: no session data found".into());
    }

    // Enrich jsonl records with the thread index metadata (title/cwd and a
    // model fallback for records whose turn had no model attached).
    for r in &mut all {
        if let Some(m) = thread_meta.get(&r.session_id) {
            if r.title.is_empty() && !m.title.is_empty() {
                r.title = m.title.clone();
            }
            if (r.cwd.is_empty() || r.cwd == ".") && !m.cwd.is_empty() {
                r.cwd = clean_cwd(&m.cwd);
            }
            if (r.model == "unknown" || r.model.is_empty()) && !m.model.is_empty() {
                r.model = m.model.clone();
                r.cost = pricing::cost(&r.model, r.input, r.output, r.cache_creation, r.cache_read);
            }
        }
    }

    // Sessions that only exist in the sqlite index (e.g. pruned rollouts)
    // still contribute a single fallback record. Sessions that already have
    // jsonl usage keep the accurate per-response records instead.
    // (jsonl files are per-response increments and may span several files
    // per session, so they are all kept; only the sqlite snapshots are
    // deduped to one record per session.)
    {
        let mut jsonl_sessions = std::collections::HashSet::new();
        for r in &all {
            jsonl_sessions.insert(r.session_id.clone());
        }
        let mut seen_db: std::collections::HashSet<String> = std::collections::HashSet::new();
        for r in db_fallback {
            if jsonl_sessions.contains(&r.session_id) {
                continue;
            }
            if !seen_db.insert(r.session_id.clone()) {
                continue;
            }
            all.push(r);
        }
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

/// Strip the `\\?\` verbatim prefix Windows APIs sometimes store in the DB.
fn clean_cwd(raw: &str) -> String {
    raw.strip_prefix(r"\\?\").unwrap_or(raw).to_string()
}

fn parse_ts(v: &serde_json::Value) -> i64 {
    if let Some(s) = v.get("timestamp").and_then(|t| t.as_str()) {
        if let Ok(d) = DateTime::parse_from_rfc3339(s) {
            return d.timestamp();
        }
    }
    if let Some(ms) = v.get("timestamp").and_then(|t| t.as_u64()) {
        return (ms / 1000) as i64;
    }
    0
}

fn num(u: &serde_json::Value, k: &str) -> u64 {
    u.get(k).and_then(|v| v.as_u64()).unwrap_or(0)
}

/// Split a Codex usage object into (input, output, cache_creation,
/// cache_read). `input_tokens` is the total including cached tokens, so the
/// cached portions are subtracted from the full-price input bucket to avoid
/// double charging (cached tokens bill at the discounted cache-read rate).
fn split_usage(u: &serde_json::Value) -> (u64, u64, u64, u64) {
    let total_in = num(u, "input_tokens");
    let cached = num(u, "cached_input_tokens");
    let cache_write = num(u, "cache_write_input_tokens");
    let output = num(u, "output_tokens");
    let input = total_in.saturating_sub(cached.saturating_add(cache_write));
    (input, output, cache_write, cached)
}

/// Parse a rollout jsonl file. Handles, in one streaming pass:
///  - `session_meta` (session cwd)
///  - `turn_context` (turn_id -> model/cwd, plus a "current model" for old
///    files whose `token_count` events carry no turn id)
///  - `token_usage_record` (new per-response `payload.usage`)
///  - `event_msg` / `token_count` (old per-response
///    `payload.info.last_token_usage`)
///  - legacy `agent_message` shapes (`payload.usage` / `payload.tokens`)
fn parse_session_file(path: &Path) -> Vec<UsageRecord> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return vec![],
    };
    let fallback_session = path
        .file_stem()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let path_str = path.to_string_lossy().to_string();
    // Rollout filenames embed the session id:
    // `rollout-<ts>-<session>.jsonl` or `rollout-<ts>-<session>_<turn>.jsonl`.
    let file_session_hint = fallback_session
        .strip_prefix("rollout-")
        .and_then(|s| {
            // strip leading timestamp `2026-09-08T23-08-59-`
            let mut parts = s.splitn(2, "-01");
            parts.next();
            parts.next().map(|rest| format!("01{}", rest))
        })
        .and_then(|s| s.split('_').next().map(|x| x.to_string()))
        .unwrap_or_default();

    let mut records: Vec<UsageRecord> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut turn_model: HashMap<String, String> = HashMap::new();
    let mut turn_cwd: HashMap<String, String> = HashMap::new();
    let mut current_model = String::new();
    let mut current_cwd = String::new();
    let mut session_cwd = String::new();
    let mut session_id_hint = String::new();

    for line in BufReader::new(file).lines().flatten() {
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        let kind = json.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let payload = json.get("payload");

        match kind {
            "session_meta" => {
                if let Some(p) = payload {
                    if let Some(id) = p.get("session_id").and_then(|v| v.as_str()) {
                        session_id_hint = id.to_string();
                    } else if let Some(id) = p.get("id").and_then(|v| v.as_str()) {
                        session_id_hint = id.to_string();
                    }
                    if let Some(cwd) = p.get("cwd").and_then(|v| v.as_str()) {
                        session_cwd = clean_cwd(cwd);
                    }
                }
                continue;
            }
            "turn_context" => {
                if let Some(p) = payload {
                    if let Some(m) = p.get("model").and_then(|v| v.as_str()) {
                        current_model = m.to_string();
                        if let Some(tid) = p.get("turn_id").and_then(|v| v.as_str()) {
                            turn_model.insert(tid.to_string(), m.to_string());
                        }
                    }
                    if let Some(cwd) = p.get("cwd").and_then(|v| v.as_str()) {
                        current_cwd = clean_cwd(cwd);
                        if let Some(tid) = p.get("turn_id").and_then(|v| v.as_str()) {
                            turn_cwd.insert(tid.to_string(), current_cwd.clone());
                        }
                    }
                }
                continue;
            }
            "token_usage_record" => {
                let Some(p) = payload else { continue };
                let Some(u) = p.get("usage") else { continue };
                let (input, output, cache_creation, cache_read) = split_usage(u);
                if input == 0 && output == 0 && cache_creation == 0 && cache_read == 0 {
                    continue;
                }
                let turn_id = p.get("turn_id").and_then(|v| v.as_str()).unwrap_or("");
                let model = turn_model
                    .get(turn_id)
                    .cloned()
                    .unwrap_or_else(|| current_model.clone());
                let model = if model.is_empty() { "unknown".to_string() } else { model };
                let sid = p
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .or_else(|| p.get("thread_id").and_then(|v| v.as_str()))
                    .map(|s| s.to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| {
                        if !session_id_hint.is_empty() {
                            session_id_hint.clone()
                        } else if !file_session_hint.is_empty() {
                            file_session_hint.clone()
                        } else {
                            fallback_session.clone()
                        }
                    });
                // Dedup the same response showing up in overlapping rollout files.
                let dedup_key = if let Some(rid) = p.get("response_id").and_then(|v| v.as_str()) {
                    format!("r:{}:{}", sid, rid)
                } else {
                    format!(
                        "t:{}:{}:{}:{}:{}:{}",
                        sid,
                        parse_ts(&json),
                        turn_id,
                        input,
                        output,
                        cache_read
                    )
                };
                if !seen.insert(dedup_key) {
                    continue;
                }
                let cwd = turn_cwd
                    .get(turn_id)
                    .cloned()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| {
                        if !current_cwd.is_empty() {
                            current_cwd.clone()
                        } else if !session_cwd.is_empty() {
                            session_cwd.clone()
                        } else {
                            path.parent()
                                .map(|p| p.to_string_lossy().to_string())
                                .unwrap_or_default()
                        }
                    });
                let ts = parse_ts(&json);
                let cost = pricing::cost(&model, input, output, cache_creation, cache_read);
                records.push(UsageRecord {
                    agent: AGENT_CODEX,
                    model,
                    ts,
                    input,
                    output,
                    cache_creation,
                    cache_read,
                    cost,
                    session_id: sid,
                    title: String::new(),
                    cwd,
                    path: path_str.clone(),
                });
                continue;
            }
            "event_msg" => {
                let Some(p) = payload else { continue };
                let ptype = p.get("type").and_then(|t| t.as_str()).unwrap_or("");
                // Old per-response shape: payload.info.last_token_usage
                if ptype == "token_count" {
                    let Some(info) = p.get("info") else { continue };
                    let Some(u) = info.get("last_token_usage") else { continue };
                    let (input, output, cache_creation, cache_read) = split_usage(u);
                    if input == 0 && output == 0 && cache_creation == 0 && cache_read == 0 {
                        continue;
                    }
                    let model = if current_model.is_empty() {
                        "unknown".to_string()
                    } else {
                        current_model.clone()
                    };
                    let sid = if !session_id_hint.is_empty() {
                        session_id_hint.clone()
                    } else if !file_session_hint.is_empty() {
                        file_session_hint.clone()
                    } else {
                        fallback_session.clone()
                    };
                    let ts = parse_ts(&json);
                    let dedup_key =
                        format!("t:{}:{}:{}:{}:{}", sid, ts, input, output, cache_read);
                    if !seen.insert(dedup_key) {
                        continue;
                    }
                    let cwd = if !current_cwd.is_empty() {
                        current_cwd.clone()
                    } else if !session_cwd.is_empty() {
                        session_cwd.clone()
                    } else {
                        path.parent()
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_default()
                    };
                    let cost = pricing::cost(&model, input, output, cache_creation, cache_read);
                    records.push(UsageRecord {
                        agent: AGENT_CODEX,
                        model,
                        ts,
                        input,
                        output,
                        cache_creation,
                        cache_read,
                        cost,
                        session_id: sid,
                        title: String::new(),
                        cwd,
                        path: path_str.clone(),
                    });
                    continue;
                }
                // Legacy agent_message shapes.
                let usage = p
                    .get("usage")
                    .or_else(|| p.get("tokens"))
                    .or_else(|| json.get("usage"));
                let Some(u) = usage else { continue };
                // Skip nested info objects; only flat token shapes here.
                if u.get("input_tokens").is_none() && u.get("input").is_none() {
                    continue;
                }
                let (input, output, cache_creation, cache_read) = if u.get("input_tokens").is_some() {
                    split_usage(u)
                } else {
                    (num(u, "input"), num(u, "output"), 0, 0)
                };
                if input == 0 && output == 0 {
                    continue;
                }
                let model = p
                    .get("model")
                    .or_else(|| json.get("model"))
                    .and_then(|m| m.as_str())
                    .unwrap_or_else(|| {
                        if current_model.is_empty() {
                            "unknown"
                        } else {
                            &current_model
                        }
                    })
                    .to_string();
                let sid = if !session_id_hint.is_empty() {
                    session_id_hint.clone()
                } else if !file_session_hint.is_empty() {
                    file_session_hint.clone()
                } else {
                    fallback_session.clone()
                };
                let ts = parse_ts(&json);
                let dedup_key =
                    format!("t:{}:{}:{}:{}:{}", sid, ts, input, output, cache_read);
                if !seen.insert(dedup_key) {
                    continue;
                }
                let cost = pricing::cost(&model, input, output, cache_creation, cache_read);
                records.push(UsageRecord {
                    agent: AGENT_CODEX,
                    model,
                    ts,
                    input,
                    output,
                    cache_creation,
                    cache_read,
                    cost,
                    session_id: sid,
                    title: String::new(),
                    cwd: if !current_cwd.is_empty() {
                        current_cwd.clone()
                    } else if !session_cwd.is_empty() {
                        session_cwd.clone()
                    } else {
                        path.parent()
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_default()
                    },
                    path: path_str.clone(),
                });
                continue;
            }
            _ => {}
        }
    }
    records
}

/// Codex index DBs: the `threads` table. `tokens_used` is an INTEGER total
/// (older `state_*.sqlite` snapshots stored a JSON `{"input":..,"output":..}`
/// string instead). Returns fallback usage records plus per-session metadata
/// used to enrich the accurate jsonl records.
fn read_threads_db(path: &Path) -> Result<(Vec<UsageRecord>, HashMap<String, ThreadMeta>), String> {
    let conn = Connection::open(path).map_err(|e| e.to_string())?;
    // Columns vary across Codex versions; select the stable core and probe
    // for the optional timestamp/model columns.
    let has_col = |name: &str| -> bool {
        conn.prepare(&format!("SELECT {} FROM threads LIMIT 0", name)).is_ok()
    };
    let ts_col = if has_col("created_at_ms") {
        "created_at_ms"
    } else if has_col("created_at") {
        // `created_at` is epoch seconds in some builds, ms in others;
        // normalized below by magnitude.
        "created_at"
    } else {
        return Err("threads table has no timestamp column".into());
    };
    let q = format!(
        "SELECT id, title, cwd, model, tokens_used, {} FROM threads",
        ts_col
    );
    let mut stmt = conn.prepare(&q).map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, rusqlite::types::Value>(4)?,
                row.get::<_, Option<i64>>(5)?,
            ))
        })
        .map_err(|e| e.to_string())?;

    let mut records = Vec::new();
    let mut meta = HashMap::new();
    for row in rows {
        let Ok((id, title, cwd, model, tokens_used, ts_raw)) = row else { continue };
        let title = title.unwrap_or_default();
        let cwd_raw = cwd.unwrap_or_default();
        let model = model.filter(|m| !m.is_empty()).unwrap_or_else(|| "unknown".into());
        meta.insert(
            id.clone(),
            ThreadMeta { title: title.clone(), cwd: cwd_raw.clone(), model: model.clone() },
        );
        let (input, output) = parse_tokens_value(&tokens_used);
        if input == 0 && output == 0 {
            continue;
        }
        let mut ts = ts_raw.unwrap_or(0);
        if ts > 10_000_000_000 {
            ts /= 1000; // ms -> s
        }
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
            session_id: id,
            title,
            cwd: clean_cwd(&cwd_raw),
            path: path.to_string_lossy().to_string(),
        });
    }
    Ok((records, meta))
}

fn parse_tokens_value(v: &rusqlite::types::Value) -> (u64, u64) {
    use rusqlite::types::Value::*;
    match v {
        Null => (0, 0),
        // An integer total has no input/output split; attribute it to input
        // so cost/token math stays in the right ballpark.
        Integer(n) => (((*n).max(0)) as u64, 0),
        Real(f) => (((*f).max(0.0)) as u64, 0),
        Text(s) => parse_tokens(Some(s)),
        Blob(_) => (0, 0),
    }
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

    #[test]
    fn parses_token_usage_record_with_turn_model() {
        let dir = std::env::temp_dir().join(format!("tt-codex-test3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("sess3.jsonl");
        let mut fh = File::create(&file).unwrap();
        writeln!(
            fh,
            r#"{{"timestamp":"2026-09-08T15:08:59.758Z","type":"session_meta","payload":{{"session_id":"sess-abc","cwd":"I:\\work"}}}}"#
        )
        .unwrap();
        writeln!(
            fh,
            r#"{{"timestamp":"2026-09-08T15:09:03.762Z","type":"turn_context","payload":{{"turn_id":"turn-1","model":"gpt-5.6-luna","cwd":"I:\\work"}}}}"#
        )
        .unwrap();
        writeln!(
            fh,
            r#"{{"timestamp":"2026-09-08T15:09:13.743Z","type":"token_usage_record","payload":{{"turn_id":"turn-1","session_id":"sess-abc","response_id":"resp-1","usage":{{"input_tokens":75234,"cached_input_tokens":17152,"cache_write_input_tokens":100,"output_tokens":229,"reasoning_output_tokens":50,"total_tokens":75463}}}}}}"#
        )
        .unwrap();
        drop(fh);

        let records = parse_session_file(&file);
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!(r.model, "gpt-5.6-luna");
        assert_eq!(r.session_id, "sess-abc");
        assert_eq!(r.cache_read, 17152);
        assert_eq!(r.cache_creation, 100);
        assert_eq!(r.input, 75234 - 17152 - 100);
        assert_eq!(r.output, 229);
        assert!(r.cost > 0.0, "gpt-5.6-luna should have a price");
        assert_eq!(r.cwd, r#"I:\work"#);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_old_token_count_shape() {
        let dir = std::env::temp_dir().join(format!("tt-codex-test4-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("sess4.jsonl");
        let mut fh = File::create(&file).unwrap();
        writeln!(
            fh,
            r#"{{"timestamp":"2025-09-15T23:46:17.114Z","type":"turn_context","payload":{{"cwd":"s:\\proj","model":"gpt-5-codex"}}}}"#
        )
        .unwrap();
        writeln!(
            fh,
            r#"{{"timestamp":"2025-09-15T23:46:23.069Z","type":"event_msg","payload":{{"type":"token_count","info":{{"last_token_usage":{{"input_tokens":3208,"cached_input_tokens":3072,"output_tokens":193}},"total_token_usage":{{"input_tokens":6295}}}}}}}}"#
        )
        .unwrap();
        drop(fh);

        let records = parse_session_file(&file);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].model, "gpt-5-codex");
        assert_eq!(records[0].cache_read, 3072);
        assert_eq!(records[0].input, 3208 - 3072);
        assert_eq!(records[0].output, 193);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn splits_cached_tokens() {
        let v: serde_json::Value =
            serde_json::from_str(r#"{"input_tokens":100,"cached_input_tokens":60,"cache_write_input_tokens":10,"output_tokens":5}"#)
                .unwrap();
        assert_eq!(split_usage(&v), (30, 5, 10, 60));
    }
}


