# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**monitord** is a Linux tool that collects systemd health metrics via D-Bus and outputs them as JSON. It monitors systemd units, networkd interfaces, PID 1 stats (via procfs), timers, containers/machines, and D-Bus daemon statistics. Configuration is INI-based (default: `/etc/monitord.conf`).

## Build & Development Commands

```bash
cargo build                          # Debug build
cargo build --release --all-features # Release build (optimized for size via LTO)
cargo test                           # Run all 23 unit tests
cargo test --lib                     # Library tests only
cargo test <test_name>               # Single test
cargo clippy                         # Lint
cargo fmt --check                    # Format check
cargo fmt                            # Auto-format
cargo run -- -c monitord.conf -l debug  # Run locally with debug logging
```

**Non-Linux development:** Use the Docker container (`docker build .` — Fedora Rawhide with systemd). Tests and runtime require systemd.

**D-Bus proxy regeneration:** `zbus-xmlgen system org.freedesktop.systemd1 /org/freedesktop/systemd1/unit/...` — output goes in `src/dbus/`.

## Architecture

Single-crate project (both library and binary). The binary (`main.rs`) parses CLI args and loads config; the library (`lib.rs::stat_collector()`) orchestrates collection.

### Data Flow

1. `main.rs` → CLI parsing (clap) + config loading (configparser INI)
2. `lib.rs::stat_collector()` → spawns collectors in parallel via `tokio::task::JoinSet`
3. Each collector communicates with systemd over D-Bus using async zbus proxies
4. Results merged into `MonitordStats` behind `Arc<RwLock<>>`
5. Output formatted as JSON (compact, pretty, or flat dot-notation) via `json.rs`

### Key Modules

- `config.rs` — INI config parsing into typed `Config` struct; controls which collectors are enabled
- `units.rs` — Systemd unit counting and per-service stats (with allowlist/blocklist)
- `networkd.rs` — systemd-networkd interface state tracking
- `pid1.rs` — PID 1 process stats from procfs (Linux-only, gated with `cfg(target_os = "linux")`)
- `system.rs` — Systemd version and system state
- `timer.rs` — Timer unit statistics
- `machines.rs` — Container stats; recursively opens D-Bus connections to each container and collects the same stat types
- `dbus_stats.rs` — D-Bus daemon statistics (supports both dbus-broker and dbus-daemon)
- `dbus/` — Auto-generated zbus proxy interfaces (via `zbus-xmlgen`); do not hand-edit

### Run Modes

- **One-shot** (default): collect once and exit (for systemd timers/cron)
- **Daemon**: continuous collection with configurable refresh interval (`daemon_stats_refresh_secs`)

## Code Conventions

- Async throughout using tokio runtime
- Error handling: `anyhow::Result` for top-level, `thiserror` for typed errors
- Enums use `strum` for string conversion and `int-enum`/`serde_repr` for integer mapping
- Tests are inline `#[cfg(test)] mod tests` blocks using `tempfile` for config fixtures
- No rustfmt.toml or clippy.toml — default settings apply
- Release profile strips symbols and uses `opt-level = "z"` (size optimization)
- Prefer IPv6 where ever possible
- Use as little deps and possible
- Keep the binary size down so we are an option for embedded systems
- Move to using varlink APIs where ever possible as they become available
