//! Durable store of scanned usage records.
//!
//! The source files (Claude Code jsonl, Codex state dbs, OpenCode's store)
//! can be pruned or rotated by the agents themselves, which would otherwise
//! shrink the app's all-time totals. Every scan upserts its records here, and
//! aggregation reads from this store so history survives source pruning.

use std::path::PathBuf;

use rusqlite::Connection;

use crate::model::UsageRecord;
use crate::sources::{AGENT_CLAUDE, AGENT_CODEX, AGENT_OPENCODE};

pub fn db_path() -> Option<PathBuf> {
    let home = crate::sources::home_dir()?;
    #[cfg(windows)]
    let base = home.join("AppData").join("Roaming");
    #[cfg(not(windows))]
    let base = home.join(".config");
    Some(base.join("TokenTracker").join("history.db"))
}

fn open() -> Result<Connection, String> {
    let path = db_path().ok_or("cannot resolve history db path")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let conn = Connection::open(&path).map_err(|e| e.to_string())?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         CREATE TABLE IF NOT EXISTS records (
             agent           TEXT    NOT NULL,
             path            TEXT    NOT NULL,
             session_id      TEXT    NOT NULL,
             ts              INTEGER NOT NULL,
             model           TEXT    NOT NULL,
             title           TEXT    NOT NULL DEFAULT '',
             cwd             TEXT    NOT NULL DEFAULT '',
             input           INTEGER NOT NULL DEFAULT 0,
             output          INTEGER NOT NULL DEFAULT 0,
             cache_creation  INTEGER NOT NULL DEFAULT 0,
             cache_read      INTEGER NOT NULL DEFAULT 0,
             cost            REAL    NOT NULL DEFAULT 0,
             PRIMARY KEY (agent, path, session_id, ts, model,
                          input, output, cache_creation, cache_read)
         );",
    )
    .map_err(|e| e.to_string())?;
    Ok(conn)
}

/// Upsert freshly scanned records. Rows are keyed on everything except
/// cost/title/cwd, so a pricing change updates the stored cost in place.
pub fn upsert(records: &[UsageRecord]) -> Result<(), String> {
    let mut conn = open()?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    {
        let mut stmt = tx
            .prepare(
                "INSERT INTO records (agent, path, session_id, ts, model, title, cwd, \
                                      input, output, cache_creation, cache_read, cost) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) \
                 ON CONFLICT (agent, path, session_id, ts, model, \
                              input, output, cache_creation, cache_read) \
                 DO UPDATE SET cost = excluded.cost, \
                               title = excluded.title, \
                               cwd = excluded.cwd",
            )
            .map_err(|e| e.to_string())?;
        for r in records {
            stmt.execute(rusqlite::params![
                r.agent,
                r.path,
                r.session_id,
                r.ts,
                r.model,
                r.title,
                r.cwd,
                r.input,
                r.output,
                r.cache_creation,
                r.cache_read,
                r.cost
            ])
            .map_err(|e| e.to_string())?;
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(())
}

/// All accumulated records, oldest first.
pub fn all() -> Result<Vec<UsageRecord>, String> {
    let conn = open()?;
    let mut stmt = conn
        .prepare(
            "SELECT agent, path, session_id, ts, model, title, cwd, \
                    input, output, cache_creation, cache_read, cost \
             FROM records ORDER BY ts",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, u64>(7)?,
                row.get::<_, u64>(8)?,
                row.get::<_, u64>(9)?,
                row.get::<_, u64>(10)?,
                row.get::<_, f64>(11)?,
            ))
        })
        .map_err(|e| e.to_string())?;

    let mut records = Vec::new();
    for row in rows {
        let Ok((agent, path, session_id, ts, model, title, cwd, input, output, cache_creation, cache_read, cost)) = row else {
            continue;
        };
        let Some(agent) = match agent.as_str() {
            "Claude Code" => Some(AGENT_CLAUDE),
            "Codex CLI" => Some(AGENT_CODEX),
            "OpenCode" => Some(AGENT_OPENCODE),
            _ => None,
        } else {
            continue;
        };
        records.push(UsageRecord {
            agent,
            model,
            ts,
            input,
            output,
            cache_creation,
            cache_read,
            cost,
            session_id,
            title,
            cwd,
            path,
        });
    }
    Ok(records)
}
