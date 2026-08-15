//! OpenCode usage parsing from its SQLite store.
//!
//! The `session` table already carries aggregated per-session usage:
//! cost, tokens_input/output/reasoning/cache_read/cache_write, model (JSON),
//! title, directory, agent, time_created (epoch ms).

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::pricing;
use crate::sources::{home_dir, AGENT_OPENCODE};

use super::UsageRecord;

pub struct Source {
    pub records: Vec<UsageRecord>,
}

#[cfg(windows)]
fn db_path() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".local").join("share").join("opencode").join("opencode.db"))
}

#[cfg(not(windows))]
fn db_path() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".local").join("share").join("opencode").join("opencode.db"))
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
            errors.push(format!("OpenCode: {}", e));
            Source { records: vec![] }
        }
    }
}

/// The beta channel writes to `session_v2` while the legacy `session` table
/// stays frozen; newer opencode versions keep both. Return the live table
/// name for a given connection.
pub fn session_table(conn: &Connection) -> &'static str {
    if has_table(conn, "session_v2") {
        "session_v2"
    } else {
        "session"
    }
}

fn has_table(conn: &Connection, name: &str) -> bool {
    matches!(
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
            rusqlite::params![name],
            |r| r.get::<_, i64>(0),
        ),
        Ok(n) if n > 0
    )
}

fn read_db(path: &Path) -> Result<Vec<UsageRecord>, String> {
    let conn = Connection::open(path).map_err(|e| e.to_string())?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| e.to_string())?;
    let db_path = path.to_string_lossy().to_string();

    // Merge `session_v2` and `session` by session id, keeping the row that
    // was updated most recently (the beta's live copy wins over the legacy
    // snapshot).
    let mut by_id: std::collections::HashMap<String, (UsageRecord, i64, bool)> =
        std::collections::HashMap::new();
    for table in ["session_v2", "session"] {
        if !has_table(&conn, table) {
            continue;
        }
        for (record, updated, is_v2) in read_table(&conn, table, &db_path)? {
            let updated = updated.max(record.ts);
            match by_id.get(&record.session_id) {
                Some((_, prev_updated, prev_v2)) if *prev_updated > updated => continue,
                Some((_, prev_updated, prev_v2)) if *prev_updated == updated && *prev_v2 && !is_v2 => continue,
                _ => {
                    by_id.insert(record.session_id.clone(), (record, updated, is_v2));
                }
            }
        }
    }

    let mut records: Vec<UsageRecord> = by_id.into_values().map(|(r, _, _)| r).collect();
    records.sort_by(|a, b| b.ts.cmp(&a.ts));
    Ok(records)
}

fn read_table(conn: &Connection, table: &str, db_path: &str) -> Result<Vec<(UsageRecord, i64, bool)>, String> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT id, title, directory, model, cost, tokens_input, tokens_output, \
             tokens_reasoning, tokens_cache_read, tokens_cache_write, time_created, time_updated, agent \
             FROM {}",
            table
        ))
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<f64>>(4)?,
                row.get::<_, Option<u64>>(5)?,
                row.get::<_, Option<u64>>(6)?,
                row.get::<_, Option<u64>>(7)?,
                row.get::<_, Option<u64>>(8)?,
                row.get::<_, Option<u64>>(9)?,
                row.get::<_, Option<i64>>(10)?,
                row.get::<_, Option<i64>>(11)?,
                row.get::<_, Option<String>>(12)?,
            ))
        })
        .map_err(|e| e.to_string())?;

    let mut out = Vec::new();
    for row in rows {
        let Ok((
            id,
            title,
            directory,
            model,
            cost,
            input,
            output,
            reasoning,
            cache_read,
            cache_write,
            time_created,
            time_updated,
            _agent,
        )) = row
        else {
            continue;
        };

        let model = model
            .and_then(|m| serde_json::from_str::<serde_json::Value>(&m).ok())
            .and_then(|m| m.get("id").and_then(|i| i.as_str()).map(|s| s.to_string()))
            .unwrap_or_else(|| "unknown".into());

        let mut input = input.unwrap_or(0);
        // reasoning tokens count as input for billing purposes in opencode
        input += reasoning.unwrap_or(0);

        // OpenCode's stored cost is zero or stale for sessions (new models,
        // old price data). Always reprice from our own fresher sheet when
        // the model resolves; fall back to the stored cost only for models
        // we don't know (free-tier models resolve to $0 and stay zero).
        let cost = if pricing::lookup(&model).is_some() {
            pricing::cost(&model, input, output.unwrap_or(0), cache_write.unwrap_or(0), cache_read.unwrap_or(0))
        } else {
            cost.unwrap_or(0.0)
        };

        out.push((
            UsageRecord {
                agent: AGENT_OPENCODE,
                model,
                ts: time_created.unwrap_or(0) / 1000,
                input,
                output: output.unwrap_or(0),
                cache_creation: cache_write.unwrap_or(0),
                cache_read: cache_read.unwrap_or(0),
                cost,
                session_id: id,
                title: title.unwrap_or_default(),
                cwd: directory.unwrap_or_default(),
                path: db_path.to_string(),
            },
            time_updated.unwrap_or(0) / 1000,
            table == "session_v2",
        ))
    }
    Ok(out)
}


