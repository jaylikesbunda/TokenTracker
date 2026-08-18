# TokenTracker

Every AI coding token and dollar your agents burn — in your system tray.

A desktop app that blends two ideas:

- **ccusage-style tracking** — parses your local agent usage data (Claude Code, Codex CLI, OpenCode) into daily/weekly/monthly totals, per-model costs and session history. No account, no API key needed.
- **CodexBar-style live limits** — shows live provider quota windows (Claude Code 5-hour/weekly tiers, Codex rate limits + credits) with reset countdowns, reusing the logins you already have.

Built with [Tauri v2](https://v2.tauri.app/) (Rust + web UI). Windows NSIS installers and Linux Debian packages are built automatically by GitHub Actions.

<img width="954" height="592" alt="image" src="https://github.com/user-attachments/assets/17767692-0c6b-48af-af6f-21c52a858a71" />


## Features

- **System tray presence** — tray icon with today's spend, left-click toggles the dashboard, close-to-tray
- **Totals** — today / this week / this month / all time, cost + tokens + sessions
- **14-day spend chart** — stacked per agent
- **Live quotas** — Claude Code OAuth usage windows (`five_hour`, `seven_day`, …) and Codex `/wham/usage` rate limits + credits, with reset countdowns
- **Per-agent cards** — models used, unpriced-model warnings, last activity, open data folder
- **Session history** — recent conversations with tokens and cost
- **Incremental scanning** — session files are cached by mtime/size; refreshes are fast
- **Privacy-first** — everything parses local files; quota checks reuse existing tokens from opencode's `auth.json`, `~/.claude/credentials.json` and `~/.codex/auth.json`. Nothing is stored or sent anywhere else.

## Data sources

| Agent | Location | Notes |
| --- | --- | --- |
| Claude Code | `~/.claude/projects/**/*.jsonl` | per-assistant-message token usage; cost computed from bundled LiteLLM pricing |
| Codex CLI | `~/.codex/sessions/**/*.jsonl` and `~/.codex/state_*.sqlite` (`threads` table) | legacy JSONL + new SQLite store |
| OpenCode | `~/.local/share/opencode/opencode.db` | session table already contains cost + tokens |

Pricing data (`src-tauri/pricing.json`) is a curated subset of LiteLLM's `model_prices_and_context_window.json`, refreshed with `python scripts/fetch_pricing.py`. Unknown models price at $0 and are flagged in the UI.

## Development

Prerequisites: [Node.js](https://nodejs.org) 18+, [Rust](https://rustup.rs) stable.

```sh
npm install
npm run tauri dev      # hot-reload dev app
npm run tauri build    # builds the platform package locally
```

Tests:

```sh
cargo test             # parser + pricing unit tests (in src-tauri)
```

Regenerate icons:

```powershell
powershell -File scripts/generate-icons.ps1
```

## Release (GitHub Actions → NSIS .exe and Debian .deb)

1. Keep the version fields synchronized for tag releases, or use the manual workflow to update them automatically.
2. Push a tag: `git tag v0.2.0 && git push origin v0.2.0`
3. The [`release` workflow](.github/workflows/release.yml) builds the NSIS installer on Windows and a Debian package on Ubuntu 22.04. Trigger it from the **Actions → release → Run workflow** page and type the version number (e.g. `0.2.0`) — it bumps the version, builds both packages, and publishes a GitHub Release. Pushing a `v*` tag also triggers the same build.

Run the workflow manually from the Actions tab to build without a tag.

## Credits

Inspired by [ccusage](https://github.com/ccusage/ccusage) (MIT) and [CodexBar](https://github.com/steipete/CodexBar) (MIT). Live quota endpoints and response shapes modeled on the [opencode-usage-plugin](https://github.com/IgorWarzocha/opencode-usage-plugin) (MIT). Pricing data from [LiteLLM](https://github.com/BerriAI/litellm) (MIT).

## License

MIT
