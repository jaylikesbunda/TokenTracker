//! Durable store of scanned usage records.
//!
//! The source files (Claude Code jsonl, Codex state dbs, OpenCode's store)
//! can be pruned or rotated by the agents themselves, which would otherwise
//! shrink the app's all-time totals. Every scan upserts its records here, and
//! aggregation reads from this store so history survives source pruning.

use std::path::PathBuf;

use rusqlite::Connection;

use crate::model::UsageRecord;
use crate::sources::{AGENT_CLAUDE, AGENT_CODEX, AGENT_COMMANDCODE, AGENT_FREEBUFF, AGENT_OPENCODE, AGENT_OSAGENT};

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
        // Snapshot sources (sqlite DBs) report cumulative per-session totals
        // that grow between scans. Keying on the counters would append a new
        // row per refresh and inflate totals ~100x, so replace each fresh
        // snapshot session's rows instead. (File-backed jsonl sources emit
        // immutable per-message rows and keep the plain upsert below.)
        {
            let mut snapshots: std::collections::HashSet<(&str, &str, &str)> =
                std::collections::HashSet::new();
            for r in records {
                let p = r.path.as_str();
                if p.ends_with(".db") || p.ends_with(".sqlite") {
                    snapshots.insert((r.agent, p, r.session_id.as_str()));
                }
            }
            if !snapshots.is_empty() {
                let mut del = tx
                    .prepare("DELETE FROM records WHERE agent = ?1 AND path = ?2 AND session_id = ?3")
                    .map_err(|e| e.to_string())?;
                for (agent, path, sid) in snapshots {
                    del.execute(rusqlite::params![agent, path, sid])
                        .map_err(|e| e.to_string())?;
                }
            }
        }
        // One-time repair: the old Codex parser stored every
        // `token_usage_record` / `token_count` row as model "unknown" with
        // the cached tokens folded into `input` (and keyed the session id
        // off the rollout filename instead of the payload). The fixed parser
        // emits the same usage with the real model, split cache buckets and
        // payload session ids, which would otherwise double-count alongside
        // the stale rows. Drop stale "unknown" rows for files this scan now
        // resolves with a real model; pruned files we can no longer re-parse
        // keep their rows. (Unknown rows a fresh scan still produces are
        // re-inserted below, so the delete is a no-op for them.)
        {
            let mut resolved: std::collections::HashSet<&str> = std::collections::HashSet::new();
            for r in records {
                if r.agent == AGENT_CODEX && r.model != "unknown" && !r.model.is_empty() {
                    resolved.insert(r.path.as_str());
                }
            }
            if !resolved.is_empty() {
                let mut del = tx
                    .prepare("DELETE FROM records WHERE agent = ?1 AND model = 'unknown' AND path = ?2")
                    .map_err(|e| e.to_string())?;
                for path in resolved {
                    del.execute(rusqlite::params![AGENT_CODEX, path])
                        .map_err(|e| e.to_string())?;
                }
            }
        }
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
        let agent = match agent.as_str() {
            "Claude Code" => AGENT_CLAUDE,
            "Codex CLI" => AGENT_CODEX,
            "OpenCode" => AGENT_OPENCODE,
            "CommandCode" => AGENT_COMMANDCODE,
            "FreeBuff" => AGENT_FREEBUFF,
            "OSAgent" => AGENT_OSAGENT,
            _ => continue,
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
