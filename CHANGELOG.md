# Changelog

## [0.1.1] - 2026-08-15

### Added
- Runtime price sheet updates: the app now fetches the latest LiteLLM pricing once per day (cached locally, bundled sheet used as fallback), so new models are priced without shipping a new release.
- Local cache invalidation after a successful price sheet refresh, so historical costs are recomputed with the updated prices.

### Changed
- Quota polling is now throttled: Claude Code / Codex / OpenCode live usage endpoints are hit at most once per 5 minutes, even when the dashboard is manually refreshed.
- Manual dashboard refresh no longer forces a full rescan; it reuses the cached result when a scan ran within the last minute.
- Failed price sheet downloads are retried at most once every 6 hours.
