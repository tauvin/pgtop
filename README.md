# pgtop

[![CI](https://github.com/tauvin/pgtop/actions/workflows/ci.yml/badge.svg)](https://github.com/tauvin/pgtop/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/pgtop.svg)](https://crates.io/crates/pgtop)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

TUI activity monitor for PostgreSQL — `htop` for your database.

`pg_stat_activity`, locks, top queries (via `pg_stat_statements`) and
replication state in a single terminal pane. Live updates over `tokio`,
non-blocking reconnect, multi-connection support and optional
cancel/terminate actions on running backends.

<!-- ![demo](docs/demo.gif) -->
<!-- Recorded via `vhs docs/demo.tape` against a live Postgres. -->

## Features

- **Live `pg_stat_activity`** — sortable, filterable backend list with state,
  wait events, runtime, current query.
- **Locks tab** — current `pg_locks` snapshot with granted/waiting state and
  blocking PID chains.
- **Top queries tab** — `pg_stat_statements` aggregates: calls, mean time,
  total time, rows. Three-state UI: loading / unavailable (extension not
  installed) / data.
- **Replication tab** — `pg_stat_replication` for primary-side observation:
  client_addr, state, lag bytes per WAL stage.
- **Header sparklines** — last 60 seconds of TPS, active connections and
  cache hit ratio.
- **Cancel / terminate** — `pg_cancel_backend` on `c`, `pg_terminate_backend`
  on `K` (with `yes`-confirmation modal). Disabled by default; opt-in via
  `--allow-actions`. All actions written to an audit log.
- **Multi-connection** — `pgtop prod staging local` opens three sessions in
  one TUI; `Alt+1..9` to switch. Each connection has its own collectors,
  filters and selection.
- **Reconnect with exponential backoff** — DB hiccups don't crash the UI;
  title shows `· connecting #N…` while retrying (500ms → 30s ceiling).
- **Profiles & layered config** — TOML profiles for prod/staging/local with
  per-profile `read_only` anti-fool flag.
- **Themes** — `dark` (default) and `light`.

## Install

### From source

```sh
cargo install pgtop
```

### Pre-built binaries

Releases on the [Releases page](https://github.com/tauvin/pgtop/releases)
include x86_64 and aarch64 binaries for Linux and macOS, built by
[`cargo-dist`](https://github.com/axodotdev/cargo-dist).

```sh
# macOS / Linux one-liner installer
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/tauvin/pgtop/releases/latest/download/pgtop-installer.sh | sh
```

## Usage

```sh
# Ad-hoc DSN
pgtop --dsn 'postgres://user:pass@host:5432/db'

# Or via DATABASE_URL
DATABASE_URL=... pgtop

# Profile from config
pgtop prod

# Multi-connection — Alt+1/Alt+2/Alt+3 switches
pgtop prod staging local

# Allow cancel/terminate (off by default)
pgtop --allow-actions
```

### Hotkeys

| Key             | Action                                              |
| --------------- | --------------------------------------------------- |
| `1` `2` `3` `4` | Switch tabs (Activity / Locks / Top Queries / Repl) |
| `↑` `↓`         | Move selection                                      |
| `Enter`         | Open backend details                                |
| `/`             | Filter (substring across user/db/query)             |
| `s` / `S`       | Cycle sort column / toggle direction                |
| `c`             | Cancel current query (`pg_cancel_backend`)          |
| `K`             | Terminate session (`pg_terminate_backend`)          |
| `Alt+1..9`      | Switch active connection (multi-conn mode)          |
| `Esc`           | Close modal                                         |
| `q` / `Ctrl+C`  | Quit                                                |

## Configuration

`pgtop` reads `~/.config/pgtop/config.toml` (or `$XDG_CONFIG_HOME/pgtop/config.toml`).

```toml
default_profile = "local"

[profiles.local]
dsn = "host=localhost user=pgtop password=pgtop port=5433 dbname=pgtop"

[profiles.prod]
dsn = "host=prod-db.example user=monitor dbname=catalog"
# read_only = true forces actions_allowed = false even with --allow-actions.
# Anti-fool for "I forgot the flag was on" when switching profiles.
read_only = true

[ui]
theme = "dark"  # or "light"

[intervals]
activity    = 1
locks       = 1
top_queries = 10
replication = 5
stats       = 1
```

Layered priority (lowest → highest): hardcoded defaults → profile
→ `DATABASE_URL` env → CLI flags.

A complete example lives in [`config.example.toml`](config.example.toml).

## TLS

Managed Postgres providers (RDS, Cloud SQL, Heroku, Supabase, …) require
TLS. pgtop honours all five libpq `sslmode` values via the DSN:

| `sslmode`     | Behaviour                                                                                       |
| ------------- | ----------------------------------------------------------------------------------------------- |
| `disable`     | No TLS.                                                                                         |
| `prefer`      | Try TLS; fall back to plain. **Default.**                                                       |
| `require`     | TLS forced. No certificate verification.                                                        |
| `verify-ca`   | TLS forced. Certificate validated against the bundled Mozilla root store (`webpki-roots`).      |
| `verify-full` | Same as `verify-ca`. (Hostname check is not yet differentiated.)                                |

```sh
pgtop --dsn 'postgres://user:pass@my-rds.amazonaws.com/db?sslmode=verify-full'
```

Custom CAs (corporate / self-signed): not supported yet — use `sslmode=require`
to skip verification, or open an issue describing your use case.

## Required permissions

Most tabs work on a vanilla read-only role:

```sql
CREATE ROLE pgtop LOGIN PASSWORD '...';
GRANT pg_read_all_stats TO pgtop;  -- or pg_monitor on PG ≥ 10
```

For Top Queries: install [`pg_stat_statements`](https://www.postgresql.org/docs/current/pgstatstatements.html)
(superuser action) and `GRANT SELECT ON pg_stat_statements TO pgtop;`.

For cancel/terminate: the role needs to be either superuser or owner of the
backend's session, or have `pg_signal_backend` granted.

## Development

```sh
# Spin up local Postgres + load generator
docker compose up -d

# Run against the local DB
cargo run -- local

# Tests + clippy + fmt enforced via pre-commit hook (cargo-husky)
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Recording the demo GIF:

```sh
brew install vhs        # or: go install github.com/charmbracelet/vhs@latest
docker compose up -d    # so pgtop has a Postgres + load generator to monitor
vhs docs/demo.tape      # produces docs/demo.gif
```

The `docs/ROADMAP.md` is the source of truth for the development plan.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual-licensed as above, without any additional terms or conditions.
