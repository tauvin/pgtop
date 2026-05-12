# Changelog

All notable changes to pgtop will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.9] — 2026-05-12

### Added

- **Export Activity to JSON.** Symmetric to the Top Queries export
  from 0.1.8: press `x` on the Activity tab to dump the current
  (filtered) snapshot to
  `~/.local/share/pgtop/exports/activity-<profile>-<timestamp>.json`.
  Each backend exports full `pg_stat_activity` columns plus two
  derived fields: `duration_secs` (= `now - query_start`) and
  `idle_secs` (= `now - state_change`, set only for backends whose
  state starts with `idle`). The latter is the headline number for
  spotting long idle-in-transaction sessions. Filter pattern (if any)
  and the unfiltered `total_count` are included as export metadata so
  it's obvious when the export is a subset.

## [0.1.8] — 2026-05-12

Top Queries quality-of-life release.

### Added

- **Export Top Queries to JSON.** Press `x` on the Top Queries tab to
  dump the current snapshot to
  `~/.local/share/pgtop/exports/top-queries-<profile>-<timestamp>.json`
  (macOS: `~/Library/Application Support/pgtop/exports/`). Each query
  is serialised with rank, full query text, calls, total/mean exec
  time, rows, and `share_of_total_time_pct` — the 80/20 view that
  answers "which query owns the runtime?" at a glance. The path
  appears in the filter line after a successful export, and the
  footer hints `x export json` whenever Top Queries is active.

### Changed

- **`<insufficient privilege>` rows are filtered out of Top Queries.**
  pg_stat_statements puts that literal in the query column for
  statements the calling role can't see (no `pg_read_all_stats` and
  not the owner). These rows aggregate every hidden statement and
  often dominate the top by total_exec_time without telling the user
  anything actionable — there's no query text to read and no pid to
  act on. Filtered at the source via
  `WHERE query <> '<insufficient privilege>'`; NULL queries excluded
  for the same reason.

## [0.1.7] — 2026-05-12

Cleanup release driven by a second three-reviewer pass over the
codebase (rust-engineer, code-quality, code-simplifier). Surfaces a
handful of silent failures, simplifies the most repetitive code, and
hardens a few low-risk-but-real edges.

### Security / robustness

- **`sslmode=verify-ca/-full` rewrite is now word-boundaried.** Was a
  naïve substring match: a DSN whose password or `application_name`
  happened to contain the literal `verify-ca` could be silently
  downgraded. Now uses a `\bsslmode=verify-(ca|full)\b` regex. Full
  DSN parsing remains deferred.
- **Audit log creation is TOCTOU-safe.** Process umask is tightened to
  `0o077` for the duration of `init_audit_log` via a RAII guard, so
  `tracing_appender` creates today's file with mode `0o600` from the
  start — no longer a window where the file is world-readable while
  `restrict_log_files` catches up.
- **EXPLAIN rejects multi-statement / truncated queries.** The query
  text comes from `pg_stat_activity.query`, so it can be malformed
  (truncated mid-statement, two statements joined). `sanitize_for_explain`
  strips a trailing `;` and refuses anything with an inner `;`.
- **`intervals.X = 0` no longer busy-loops.** Zero-second config values
  now fall back to the default with a `warn!` log instead of pinning a
  CPU and hammering Postgres.

### Surface silent failures

- **`SET application_name = 'pgtop'`** failures are now logged at
  `warn` instead of `let _ = ...` — without that, `is_self()` returns
  false for our own connections and they appear in the Activity table.
- **Action command `send`** failures (cancel/terminate dropped because
  the executor task exited) are logged at `warn`, so the UI's "no
  result yet" state isn't completely silent.
- **`as u32`** lossy cast in `fetch_raw_stats` replaced with
  `try_from().unwrap_or(0)`. Defence against the SQL changing — today
  the column is non-negative by construction.

### Simplifications (internal)

- **`select_previous` / `select_next`** in `ConnectionState` collapsed
  from ~160 lines of per-tab match arms to ~25 via a `tab_table(tab) ->
  Option<(&mut TableState, usize)>` router.
- **Four pure collectors** (locks, replication, tables, top_queries)
  now wrap a generic `run_simple_collector<F, T>` driver via
  `AsyncFn(&Client) -> Result<T, DbError>`. ~200 lines removed.
  Activity/databases/stats keep bespoke loops because of state hooks.
- **`recompute_filtered`** uses the existing `clamp_table_state`
  helper instead of inlining the same four-arm match.
- **`stats` collector** consolidates `prev_xacts`/`prev_time` into one
  `Option<(i64, Instant)>` — eliminates an implicit "update together"
  invariant.
- **`maybe_close_dead_modal`** merges three pid-bearing `Mode` arms
  into one.

### Polish

- `Cargo.toml`: `tokio = "full"` → explicit features
  (`rt-multi-thread`, `macros`, `signal`, `sync`, `time`).
- `views/EM_DASH` shared `&'static str` consolidates three local
  `em_dash() -> String` helpers.
- `webpki_roots_store` cached in a `OnceLock<Arc<RootCertStore>>` —
  was rebuilt from `TLS_SERVER_ROOTS` on every connect.
- Footer separator constant.

### Tests / CI

- 5 new tests (4 for `sanitize_for_explain`, 1 for the password-
  substring case of `rewrite_verify_sslmode`). Total: 71.

## [0.1.6] — 2026-05-11

Phase 13 internal refactor release driven by the architect's
sequencing recommendation. One small user-visible feature; the rest is
plumbing that pays back the next time someone touches the codebase.

### Added

- **Per-database TPS in the Databases tab.** A new `tps` column shows
  the rate of commits + rollbacks per database, computed in the
  collector from the delta between consecutive samples. First sample
  is `—`; subsequent samples render the rate. Counter resets
  (`pg_stat_reset()`, drop+recreate) gracefully fall back to one `—`
  before re-baselining.

### Changed (internal)

- **Time-injection through the render path.** `ui::render` now takes
  `now: DateTime<Utc>` and threads it to `count_slow`, `tab_suffix`,
  `title_for`, `render_activity`, `render_tables`. Single `Utc::now()`
  call lives in `run_event_loop` — the render path is deterministic
  and snapshot-testable.
- **UI snapshot tests via `insta` + `ratatui::TestBackend`.** Seven
  snapshots at 120×24 cover Activity (populated and empty), Locks,
  Databases, Tables, Detail popup, ConfirmTerminate popup. Changes to
  layout, formatting, or colours surface as diffs.
- **`src/app.rs` (~1300 lines) split** by data ownership into
  `app/mod.rs` (App, Mode, ExplainPopup), `app/connection.rs`
  (ConnectionState, ConnectionStatus, Filter, StatsHistory, WaitRow),
  and `app/tab.rs` (Tab, SortBy, SortDirection). External call sites
  unchanged thanks to `pub use` re-exports.
- **Per-mode keymap handlers.** The 110-line nested match in
  `run_event_loop` is now a 17-line dispatch to one of seven
  `handle_<mode>_key` functions, each returning `ControlFlow<()>`.
  Adding a new mode is a new handler + one match arm.
- **Borrowed `Row<'a>` in Activity render.** `backend_to_row` returns
  `Row<'a>` with `Cow<'a, str>` cells — usename / state / wait / query
  are borrowed instead of cloned every frame. Disjoint-field borrow
  (`&conn.backends` + `&mut conn.table_state`) bypasses the self-method
  block.
- **`Tab` and `SortBy` via `strum` derive.** `EnumIter`, `IntoStaticStr`,
  `EnumString` replace the four parallel manual matches over variants.
  `Tab::label()` stays manual because `Top Queries` has a space.

### Tests / CI

- 7 new UI snapshot tests, 4 new TPS-collector tests, 3 new round-trip
  tests for `Tab::{id,from_id,index,from_index}` and `SortBy::{label,
  from_label}`, plus 1 test for the new `format_tps` formatter.
  Total: 66 tests (was 52).

## [0.1.5] — 2026-05-10

Hardening release driven by a code review of 0.1.4. No new features —
fixes and observability across security, correctness, and ergonomics.

### Security

- **`sslmode=verify-ca` now skips hostname verification**, matching
  libpq's documented behaviour. Previously verify-ca was silently
  treated as verify-full (chain + hostname). A new `ChainOnlyVerifier`
  wraps `WebPkiServerVerifier` and ignores `NotValidForName` errors,
  while `verify-full` retains the full check.
- **EXPLAIN now sets `statement_timeout = '5s'`** before running the
  plan and issues a Postgres-protocol `CancelRequest` via
  `Client::cancel_token` when the user closes the popup. Previously
  `client.query` already in flight could not be cancelled and a
  pathological planner could wedge the connection indefinitely.
- **Audit log split off into its own sink**: `pgtop-audit.log` (in the
  state directory) only receives `target = "audit"` events regardless
  of `RUST_LOG`, so users can't accidentally silence the audit trail
  by raising the log level. Both audit and app logs are rotated daily
  and created with mode `0600` on Unix; the log directory is `0700`.

### Correctness

- **Per-connection `last_action_result`**: action results from a
  background connection are no longer dropped when the user is on a
  different connection. Switching to that connection surfaces the
  result.
- **Cancellation lifecycle for the EXPLAIN popup**: switching
  connections (Alt+N) or closing the popup now aborts the in-flight
  task via a per-popup child token, instead of leaving it running.
- **Collector errors are logged**: the catch-all `Err(_)` arms in all
  seven collectors emit `tracing::warn!` with `collector`, `conn_idx`,
  and `error` fields. Previously transient query errors (revoked
  permissions, statement timeout, etc.) were silently swallowed and
  the UI kept showing stale data with no signal.
- **Shutdown timeout**: `join_all` on collector handles is now wrapped
  in a 2 s timeout. A wedged task (e.g. tokio-postgres connect blocked
  on slow DNS resolution) no longer keeps the process alive after the
  user pressed `q`.

### UX

- **Filter matches more fields**: `Filter::matches` now checks query,
  username, state, and database name. `/alice` finally works as a
  username filter.
- **Removed silent fallback to a docker-compose dev DSN**: with no
  CLI flag, no `DATABASE_URL`, and no profile, pgtop used to try
  `postgres://pgtop:pgtop@localhost:5433/pgtop` and report
  "Connection refused". Now it errors out with a message naming the
  three ways to provide a DSN.

### Tests / CI

- 7 new unit tests for `Resolved::from_layers` covering the priority
  chain (CLI > env > profile), profile-not-found errors, and the
  `actions_allowed = cli_allow_actions && !read_only` interaction
  including the read-only-profile anti-fool seal.
- 2 new tests for the expanded `Filter`.
- `msrv` CI job now runs `cargo test` in addition to `cargo build`,
  catching tests that quietly pull in newer-than-MSRV API.

## [0.1.4] — 2026-05-10

### Added

- **Mouse scroll-wheel** for navigation. ScrollUp / ScrollDown move
  the table selection on every tab. Click-to-select is not
  implemented yet.
- **Jump-to-pid** — press `g` on the Activity tab, type the pid
  digits, Enter selects that row in the filtered list, Esc cancels.
- **Persisted UI state** — last tab, filter pattern, sort column and
  direction are saved on graceful exit and restored on next startup.
  File location follows the platform data dir convention
  (`~/.local/share/pgtop/state.toml` on Linux,
  `~/Library/Application Support/pgtop/state.toml` on macOS). Best-
  effort — load / save errors are logged via tracing and never block.
- **Docker image** at `ghcr.io/tauvin/pgtop` — multi-arch
  (linux/amd64, linux/arm64), built and pushed by GitHub Actions on
  each `v*` tag. Multi-stage Dockerfile, `ca-certificates` included
  for managed-DB TLS.
- **Homebrew formula** — `dist-workspace.toml` configured with the
  homebrew installer pointing at `tauvin/homebrew-pgtop`. Tap repo
  and `HOMEBREW_TAP_TOKEN` secret are user-side prerequisites; once
  set up, every release auto-updates the formula.
- **Nix flake** — `flake.nix` builds pgtop via `crane`. Users can
  `nix run github:tauvin/pgtop -- --dsn '...'` directly.

### Changed

- Mouse capture is now enabled while pgtop is running. Hold `Shift`
  to bypass capture and let the terminal handle text selection
  (works in iTerm, Terminal.app, Alacritty, Kitty, GNOME Terminal).

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

[Unreleased]: https://github.com/tauvin/pgtop/compare/v0.1.9...HEAD
[0.1.9]: https://github.com/tauvin/pgtop/compare/v0.1.8...v0.1.9
[0.1.8]: https://github.com/tauvin/pgtop/compare/v0.1.7...v0.1.8
[0.1.7]: https://github.com/tauvin/pgtop/compare/v0.1.6...v0.1.7
[0.1.6]: https://github.com/tauvin/pgtop/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/tauvin/pgtop/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/tauvin/pgtop/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/tauvin/pgtop/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/tauvin/pgtop/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/tauvin/pgtop/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/tauvin/pgtop/releases/tag/v0.1.0
