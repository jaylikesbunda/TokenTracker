//! OpenCode usage parsing from its SQLite store.
//!
//! The `session` table already carries aggregated per-session usage:
//! cost, tokens_input/output/reasoning/cache_read/cache_write, model (JSON),
//! title, directory, agent, time_created (epoch ms).

use std::path::{Path, PathBuf};

use rusqlite::Connection;

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

fn read_db(path: &Path) -> Result<Vec<UsageRecord>, String> {
    let conn = Connection::open(path).map_err(|e| e.to_string())?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| e.to_string())?;

    let mut stmt = conn
        .prepare(
            "SELECT id, title, directory, model, cost, tokens_input, tokens_output, \
             tokens_reasoning, tokens_cache_read, tokens_cache_write, time_created, agent \
             FROM session",
        )
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
                row.get::<_, Option<String>>(11)?,
            ))
        })
        .map_err(|e| e.to_string())?;

    let mut records = Vec::new();
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
            _agent,
        )) = row
        else {
            continue;
        };

        let model = model
            .and_then(|m| serde_json::from_str::<serde_json::Value>(&m).ok())
            .and_then(|m| m.get("id").and_then(|i| i.as_str()).map(|s| s.to_string()))
            .unwrap_or_else(|| "unknown".into());

        records.push(UsageRecord {
            agent: AGENT_OPENCODE,
            model,
            ts: time_created.unwrap_or(0) / 1000,
            input: input.unwrap_or(0),
            output: output.unwrap_or(0),
            cache_creation: cache_write.unwrap_or(0),
            cache_read: cache_read.unwrap_or(0),
            cost: cost.unwrap_or(0.0),
            session_id: id,
            title: title.unwrap_or_default(),
            cwd: directory.unwrap_or_default(),
            path: path.to_string_lossy().to_string(),
        });

        // reasoning tokens count as input for billing purposes in opencode;
        // keep them separate from display input by folding into input.
        if let Some(r) = records.last_mut() {
            r.input += reasoning.unwrap_or(0);
        }
    }
    Ok(records)
}


