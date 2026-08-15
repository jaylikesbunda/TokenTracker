# Changelog

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
