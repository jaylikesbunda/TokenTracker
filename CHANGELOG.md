# Changelog

## [0.3.0] - 2026-09-10

### Added
- CommandCode CLI tracking: per-message `usage` (with billed `costUsd`) from `~/.commandcode/projects/**/*.jsonl`, titles from sibling `.meta.json` files.
- OSAgent tracking: per-message tokens from `~/.osagent/osagent.db` (`session_transcript` joined to `sessions` model/title).
- FreeBuff tracking: per-turn usage from `~/.config/freebuff-desktop/projects/*/desktop-v2.db` (assistant `messages.metrics_json.usage`).
- Codex CLI now parses the current rollout format: per-response `token_usage_record` events with the model resolved via surrounding `turn_context` entries, plus legacy `token_count` (`payload.info.last_token_usage`) events. Cached tokens are split into their own buckets and session ids come from the payload instead of the filename.

### Fixed
- Codex sessions no longer show up as `unknown` model with $0.00 (e.g. a 129M-token session that previously priced at zero now attributes to `gpt-5.6-luna`/`gpt-5.6-sol`).
- History no longer accumulates a fresh cumulative snapshot of every running sqlite-backed session on each refresh, which inflated OpenCode all-time totals ~29x (and produced a phantom $2,540 day). Snapshot sources (`.db`/`.sqlite`) now replace their per-session rows instead of appending.
- One-time repair drops stale `unknown`-model Codex rows for files the fixed parser now resolves.
- Codex `state_*.sqlite` index is now only a fallback for sessions with no jsonl usage (plus title/cwd/model enrichment) instead of competing with the accurate per-response records.

### Changed
- `openrouter/free` is treated as a free-tier model so it is no longer flagged unpriced.

## [0.1.1] - 2026-08-15

### Added
- Runtime price sheet updates: the app now fetches the latest LiteLLM pricing once per day (cached locally, bundled sheet used as fallback), so new models are priced without shipping a new release.
- Local cache invalidation after a successful price sheet refresh, so historical costs are recomputed with the updated prices.
- Durable history store (`%APPDATA%\TokenTracker\history.db`): scanned usage is persisted every refresh, so all-time totals no longer shrink when Claude Code / Codex / OpenCode prune or rotate their own session files.

### Fixed
- All-time totals no longer double-count Codex sessions that exist in both the legacy jsonl and `state_*.sqlite` snapshots, and no longer jump when old Codex state files are removed.
- All-time session count is no longer capped at 100 (it previously used the truncated recent-sessions list).
- A single malformed model entry in the LiteLLM sheet no longer aborts the whole price refresh.
- OpenCode sessions are now always priced from TokenTracker's own sheet (falling back to the stored cost only for unknown models), so stale or zero stored costs — e.g. `gpt-5.6-sol` sessions OpenCode ran before it knew the price — no longer show $0.00.
- OpenCode beta sessions are now tracked: the beta channel writes to the `session_v2` table (newer opencode schema) while the legacy `session` table stays frozen. Both tables are read and merged per session, preferring the live copy.

### Changed
- Quota polling is now throttled: Claude Code / Codex / OpenCode live usage endpoints are hit at most once per 5 minutes, even when the dashboard is manually refreshed.
- Manual dashboard refresh no longer forces a full rescan; it reuses the cached result when a scan ran within the last minute.
- Failed price sheet downloads are retried at most once every 6 hours.
