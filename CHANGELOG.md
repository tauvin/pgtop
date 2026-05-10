# Changelog

All notable changes to pgtop will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1] — 2026-05-10

### Fixed

- Lowered the declared MSRV from 1.95 to 1.88 (`let-chains` is the newest
  feature actually in use). 0.1.0 was unbuildable on the GitHub Actions
  release runners, which ship rustc 1.93.1, and excluded users on stable
  releases between 1.88 and 1.94.

## [0.1.0] — 2026-05-10

Initial release.

### Added

- **`pg_stat_activity` view** — sortable, filterable backend list with state,
  wait events, runtime and current query. Row-level colour coding by state
  and runtime; live updates at 1 Hz.
- **Locks tab** — `pg_locks` snapshot with granted/waiting state and blocking
  PID information.
- **Top Queries tab** — `pg_stat_statements` aggregates with three-state UI
  (loading / unavailable / data) so a missing extension renders a hint
  instead of an error.
- **Replication tab** — `pg_stat_replication` for primary-side observation
  with per-stage WAL lag bytes.
- **Header sparklines** — last 60 seconds of TPS, active connections and
  cache hit ratio.
- **Cancel / terminate actions** — `pg_cancel_backend` (`c`) and
  `pg_terminate_backend` (`K` + `yes`-confirmation modal). Off by default,
  opt-in via `--allow-actions`. Anti-fool `read_only` profile flag forces
  actions disabled even when the flag is on. All actions written to a
  separate audit log file.
- **Multi-connection support** — `pgtop prof1 prof2 ...` opens N sessions
  in one TUI; `Alt+1..9` switches between them. Each connection has its
  own collectors, filters, selection and sort state. Title bar shows
  `· N/M` indicator.
- **Reconnect with exponential backoff** — collectors and the action
  executor own their connection lifecycle. On disconnect they retry with
  500 ms → 30 s ceiling backoff, cancellable via the shutdown token. Title
  bar shows `· connecting #N…` while the active connection isn't healthy.
  pgtop now starts cleanly even when the database is unreachable.
- **TOML profiles & layered config** — `~/.config/pgtop/config.toml` with
  named profiles. Layered priority: hardcoded defaults → profile →
  `DATABASE_URL` env → CLI flags. Per-profile `read_only`, custom poll
  intervals and theme.
- **Themes** — `dark` (default) and `light`.
- **CLI** — `clap` derive parser with `--dsn`, `--allow-actions`,
  `--read-only`, positional `[PROFILES]...`.
- **Logging** — `tracing` to file via `tracing-appender` (non-blocking),
  separate `audit` target for cancel/terminate events.
- **Graceful shutdown** — `CancellationToken` propagation; terminal restored
  before background tasks are awaited so the user doesn't see a frozen
  frame during teardown.

[Unreleased]: https://github.com/tauvin/pgtop/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/tauvin/pgtop/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/tauvin/pgtop/releases/tag/v0.1.0
