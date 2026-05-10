# Changelog

All notable changes to pgtop will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.3] — 2026-05-10

### Added

- **Databases tab** (hotkey `5`) — per-database snapshot from
  `pg_stat_database`: connection count, cumulative commits and
  rollbacks (with K/M/B suffixes), cache hit ratio, temp bytes,
  deadlocks. Sorted by transaction volume so the busiest databases
  surface first. Default 5 s poll, configurable via `[intervals]
  databases`.
- **Tables tab** (hotkey `6`) — top 50 user tables from
  `pg_stat_user_tables` ordered by dead-tuple count: live and dead
  tuple counts, dead %, last vacuum and analyze (most recent of manual
  / autovacuum, formatted as `5m` / `2h` / `3d`), sequential vs index
  scan counts. Default 10 s poll, configurable via `[intervals]
  tables`.
- **Waits histogram tab** (hotkey `7`) — sampling-based aggregation of
  `(wait_event_type, wait_event)` pairs from the latest activity
  snapshot. Shows count and share, sorted descending. No extra SQL —
  reuses the activity collector's data.
- **EXPLAIN popup** — press `e` on a selected backend in the Activity
  tab to run `EXPLAIN <query>` on a one-shot ad-hoc connection. Plain
  EXPLAIN (not `ANALYZE`) so it stays read-only and safe against
  production. Esc / q closes the popup.
- **Slow-query alerts** — new top-level config key
  `slow_query_threshold_secs` (default 30). Active backends running
  longer than the threshold are rendered in red+bold and surfaced as
  `⚠ N slow` in the Activity tab title.

## [0.1.2] — 2026-05-10

### Added

- **TLS support** via `tokio-postgres-rustls`. The `sslmode` value in the
  DSN now drives the connector: `disable`, `prefer` (default), `require`
  pass through to tokio-postgres directly with no certificate
  verification (matching libpq); `verify-ca` and `verify-full` are
  rewritten to `require` and turn on certificate verification against
  the bundled Mozilla root store (`webpki-roots`). Hostname check is not
  yet differentiated between verify-ca and verify-full. This unblocks
  managed Postgres providers (RDS, Cloud SQL, Heroku, Supabase, …).
- 28 unit tests covering `Filter`, `SortBy` / `SortDirection`,
  `compare_backends`, and the view-level formatters
  (`format_wait` / `format_duration` / `format_query` / `format_lag`).
- `msrv` CI job that builds against rustc 1.88, the declared MSRV, so a
  future code change that quietly relies on a newer feature is caught
  before release.
- `docs/demo.tape` — vhs script for recording the README demo GIF.

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

[Unreleased]: https://github.com/tauvin/pgtop/compare/v0.1.3...HEAD
[0.1.3]: https://github.com/tauvin/pgtop/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/tauvin/pgtop/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/tauvin/pgtop/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/tauvin/pgtop/releases/tag/v0.1.0
