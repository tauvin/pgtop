//! Top Queries snapshot export.
//!
//! The Top Queries tab (`pg_stat_statements`) shows a snapshot that's
//! often worth handing off for further analysis — to a teammate, to a
//! ticket, or to an LLM. This module serialises the active connection's
//! snapshot to a self-contained JSON file with a timestamp in its name so
//! exports never overwrite each other.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::app::WaitRow;
use crate::db::{Backend, DatabaseStat, Lock, Replica, TableStat, TopQuery};

/// One row in the exported JSON. `share_of_total_time_pct` is computed
/// against the sum of `total_exec_time_ms` across the snapshot — the
/// 80/20 view that's usually the first question ("which query owns the
/// runtime?").
#[derive(Serialize)]
struct ExportedQuery<'a> {
    rank: usize,
    query: &'a str,
    calls: i64,
    total_exec_time_ms: f64,
    mean_exec_time_ms: f64,
    rows: i64,
    share_of_total_time_pct: f64,
}

#[derive(Serialize)]
struct Export<'a> {
    generated_at: DateTime<Utc>,
    profile: Option<&'a str>,
    queries: Vec<ExportedQuery<'a>>,
}

/// Render the snapshot to a JSON string. Returns the body and the
/// timestamp used in the suggested filename.
pub fn render_json(
    queries: &[TopQuery],
    profile: Option<&str>,
    now: DateTime<Utc>,
) -> serde_json::Result<String> {
    let total: f64 = queries.iter().map(|q| q.total_exec_time_ms).sum();
    let exported: Vec<ExportedQuery<'_>> = queries
        .iter()
        .enumerate()
        .map(|(i, q)| ExportedQuery {
            rank: i + 1,
            query: &q.query,
            calls: q.calls,
            total_exec_time_ms: q.total_exec_time_ms,
            mean_exec_time_ms: q.mean_exec_time_ms,
            rows: q.rows,
            share_of_total_time_pct: if total > 0.0 {
                100.0 * q.total_exec_time_ms / total
            } else {
                0.0
            },
        })
        .collect();
    let export = Export {
        generated_at: now,
        profile,
        queries: exported,
    };
    serde_json::to_string_pretty(&export)
}

/// Suggested output directory (`$XDG_DATA_HOME/pgtop/exports` on Linux,
/// `~/Library/Application Support/pgtop/exports` on macOS). Created on
/// demand.
pub fn export_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("pgtop")
        .join("exports")
}

/// Build a timestamped export path. `kind` distinguishes export shapes
/// (`"top-queries"` vs `"activity"`); the profile (if any) is included
/// for at-a-glance recognition.
fn timestamped_path(kind: &str, profile: Option<&str>, now: DateTime<Utc>) -> PathBuf {
    let stamp = now.format("%Y%m%d-%H%M%S");
    let name = match profile {
        Some(p) => format!("{kind}-{p}-{stamp}.json"),
        None => format!("{kind}-{stamp}.json"),
    };
    export_dir().join(name)
}

pub fn export_path(profile: Option<&str>, now: DateTime<Utc>) -> PathBuf {
    timestamped_path("top-queries", profile, now)
}

pub fn activity_export_path(profile: Option<&str>, now: DateTime<Utc>) -> PathBuf {
    timestamped_path("activity", profile, now)
}

pub fn locks_export_path(profile: Option<&str>, now: DateTime<Utc>) -> PathBuf {
    timestamped_path("locks", profile, now)
}

pub fn databases_export_path(profile: Option<&str>, now: DateTime<Utc>) -> PathBuf {
    timestamped_path("databases", profile, now)
}

pub fn tables_export_path(profile: Option<&str>, now: DateTime<Utc>) -> PathBuf {
    timestamped_path("tables", profile, now)
}

pub fn replication_export_path(profile: Option<&str>, now: DateTime<Utc>) -> PathBuf {
    timestamped_path("replication", profile, now)
}

pub fn waits_export_path(profile: Option<&str>, now: DateTime<Utc>) -> PathBuf {
    timestamped_path("waits", profile, now)
}

/// Write the snapshot to disk, creating the export dir if needed.
/// Returns the path on success.
pub fn write(
    queries: &[TopQuery],
    profile: Option<&str>,
    now: DateTime<Utc>,
) -> std::io::Result<PathBuf> {
    let body = render_json(queries, profile, now)
        .map_err(|e| std::io::Error::other(format!("serialise top queries: {e}")))?;
    let dir = export_dir();
    std::fs::create_dir_all(&dir)?;
    let path = export_path(profile, now);
    std::fs::write(&path, body)?;
    Ok(path)
}

/// One backend in the Activity export. Mirrors the columns shown in
/// pg_stat_activity, plus two derived fields (`duration_secs`,
/// `idle_secs`) that fold the timestamps against `generated_at` so the
/// reader doesn't have to do the arithmetic.
#[derive(Serialize)]
struct ExportedBackend<'a> {
    pid: i32,
    datname: Option<&'a str>,
    usename: Option<&'a str>,
    application_name: Option<&'a str>,
    client_addr: Option<&'a str>,
    state: Option<&'a str>,
    wait_event_type: Option<&'a str>,
    wait_event: Option<&'a str>,
    backend_type: Option<&'a str>,
    /// `application_name == "pgtop"` — pgtop's own connections. Excluded
    /// from cancel/terminate actions in the TUI; flagged here so the
    /// reader can do the same.
    is_self: bool,
    /// `(generated_at - query_start).num_seconds()`. For active backends
    /// this is the current query duration; for idle ones, the duration
    /// of the most recent query before the connection went idle.
    duration_secs: Option<i64>,
    /// `(generated_at - state_change).num_seconds()` for any state that
    /// starts with `idle`. Long idle-in-transaction is usually the
    /// problem you're hunting.
    idle_secs: Option<i64>,
    backend_start: Option<DateTime<Utc>>,
    xact_start: Option<DateTime<Utc>>,
    query_start: Option<DateTime<Utc>>,
    state_change: Option<DateTime<Utc>>,
    backend_xid: Option<&'a str>,
    backend_xmin: Option<&'a str>,
    query: Option<&'a str>,
}

#[derive(Serialize)]
struct ActivityExport<'a> {
    generated_at: DateTime<Utc>,
    profile: Option<&'a str>,
    /// Active filter regex when the export was triggered (`None` if no
    /// filter is set). Helpful context when the exported set is a
    /// subset of all live backends.
    filter: Option<&'a str>,
    /// Number of backends in `backends` — i.e. what the user saw.
    exported_count: usize,
    /// Total number of backends in the snapshot before the filter was
    /// applied. Equal to `exported_count` when no filter is set.
    total_count: usize,
    backends: Vec<ExportedBackend<'a>>,
}

fn to_exported_backend<'a>(b: &'a Backend, now: DateTime<Utc>) -> ExportedBackend<'a> {
    let duration_secs = b.query_start.map(|t| (now - t).num_seconds());
    let is_idle = b.state.as_deref().is_some_and(|s| s.starts_with("idle"));
    let idle_secs = if is_idle {
        b.state_change.map(|t| (now - t).num_seconds())
    } else {
        None
    };
    ExportedBackend {
        pid: b.pid,
        datname: b.datname.as_deref(),
        usename: b.usename.as_deref(),
        application_name: b.application_name.as_deref(),
        client_addr: b.client_addr.as_deref(),
        state: b.state.as_deref(),
        wait_event_type: b.wait_event_type.as_deref(),
        wait_event: b.wait_event.as_deref(),
        backend_type: b.backend_type.as_deref(),
        is_self: b.is_self(),
        duration_secs,
        idle_secs,
        backend_start: b.backend_start,
        xact_start: b.xact_start,
        query_start: b.query_start,
        state_change: b.state_change,
        backend_xid: b.backend_xid.as_deref(),
        backend_xmin: b.backend_xmin.as_deref(),
        query: b.query.as_deref(),
    }
}

/// Render an Activity snapshot. `backends` should be the filtered slice
/// the user is looking at; `total_count` is the unfiltered backend count.
pub fn render_activity_json(
    backends: &[&Backend],
    total_count: usize,
    profile: Option<&str>,
    filter: Option<&str>,
    now: DateTime<Utc>,
) -> serde_json::Result<String> {
    let exported: Vec<ExportedBackend<'_>> = backends
        .iter()
        .map(|b| to_exported_backend(b, now))
        .collect();
    let export = ActivityExport {
        generated_at: now,
        profile,
        filter,
        exported_count: exported.len(),
        total_count,
        backends: exported,
    };
    serde_json::to_string_pretty(&export)
}

pub fn write_activity(
    backends: &[&Backend],
    total_count: usize,
    profile: Option<&str>,
    filter: Option<&str>,
    now: DateTime<Utc>,
) -> std::io::Result<PathBuf> {
    let body = render_activity_json(backends, total_count, profile, filter, now)
        .map_err(|e| std::io::Error::other(format!("serialise activity: {e}")))?;
    let dir = export_dir();
    std::fs::create_dir_all(&dir)?;
    let path = activity_export_path(profile, now);
    std::fs::write(&path, body)?;
    Ok(path)
}

/// One row in the Locks export.
#[derive(Serialize)]
struct ExportedLock<'a> {
    pid: i32,
    locktype: &'a str,
    mode: &'a str,
    granted: bool,
    object: Option<&'a str>,
}

#[derive(Serialize)]
struct LocksExport<'a> {
    generated_at: DateTime<Utc>,
    profile: Option<&'a str>,
    exported_count: usize,
    /// Number of locks with `granted = true`.
    granted_count: usize,
    /// Number of locks waiting (`granted = false`). Non-zero values are
    /// the interesting case — there's at least one blocked backend.
    waiting_count: usize,
    locks: Vec<ExportedLock<'a>>,
}

pub fn render_locks_json(
    locks: &[Lock],
    profile: Option<&str>,
    now: DateTime<Utc>,
) -> serde_json::Result<String> {
    let waiting_count = locks.iter().filter(|l| !l.granted).count();
    let exported: Vec<ExportedLock<'_>> = locks
        .iter()
        .map(|l| ExportedLock {
            pid: l.pid,
            locktype: &l.locktype,
            mode: &l.mode,
            granted: l.granted,
            object: l.object.as_deref(),
        })
        .collect();
    let export = LocksExport {
        generated_at: now,
        profile,
        exported_count: exported.len(),
        granted_count: exported.len() - waiting_count,
        waiting_count,
        locks: exported,
    };
    serde_json::to_string_pretty(&export)
}

pub fn write_locks(
    locks: &[Lock],
    profile: Option<&str>,
    now: DateTime<Utc>,
) -> std::io::Result<PathBuf> {
    let body = render_locks_json(locks, profile, now)
        .map_err(|e| std::io::Error::other(format!("serialise locks: {e}")))?;
    let dir = export_dir();
    std::fs::create_dir_all(&dir)?;
    let path = locks_export_path(profile, now);
    std::fs::write(&path, body)?;
    Ok(path)
}

/// One row in the Databases export. The collector-derived `tps` is
/// included; total/rollback counters are cumulative since the last
/// `pg_stat_reset()`.
#[derive(Serialize)]
struct ExportedDatabase<'a> {
    datname: &'a str,
    numbackends: i32,
    /// Computed by the databases collector from the delta between
    /// consecutive snapshots; `None` for the first sample after a
    /// (re)connect.
    tps: Option<f64>,
    xact_commit: i64,
    xact_rollback: i64,
    blks_hit: i64,
    blks_read: i64,
    cache_hit_pct: f64,
    temp_bytes: i64,
    deadlocks: i64,
}

#[derive(Serialize)]
struct DatabasesExport<'a> {
    generated_at: DateTime<Utc>,
    profile: Option<&'a str>,
    exported_count: usize,
    databases: Vec<ExportedDatabase<'a>>,
}

pub fn render_databases_json(
    dbs: &[DatabaseStat],
    profile: Option<&str>,
    now: DateTime<Utc>,
) -> serde_json::Result<String> {
    let exported: Vec<ExportedDatabase<'_>> = dbs
        .iter()
        .map(|d| ExportedDatabase {
            datname: &d.datname,
            numbackends: d.numbackends,
            tps: d.tps,
            xact_commit: d.xact_commit,
            xact_rollback: d.xact_rollback,
            blks_hit: d.blks_hit,
            blks_read: d.blks_read,
            cache_hit_pct: d.cache_hit_pct(),
            temp_bytes: d.temp_bytes,
            deadlocks: d.deadlocks,
        })
        .collect();
    let export = DatabasesExport {
        generated_at: now,
        profile,
        exported_count: exported.len(),
        databases: exported,
    };
    serde_json::to_string_pretty(&export)
}

pub fn write_databases(
    dbs: &[DatabaseStat],
    profile: Option<&str>,
    now: DateTime<Utc>,
) -> std::io::Result<PathBuf> {
    let body = render_databases_json(dbs, profile, now)
        .map_err(|e| std::io::Error::other(format!("serialise databases: {e}")))?;
    let dir = export_dir();
    std::fs::create_dir_all(&dir)?;
    let path = databases_export_path(profile, now);
    std::fs::write(&path, body)?;
    Ok(path)
}

/// One row in the Tables export. `dead_pct` is materialised so the
/// reader doesn't have to recompute it from live/dead tuples.
#[derive(Serialize)]
struct ExportedTable<'a> {
    schemaname: &'a str,
    relname: &'a str,
    n_live_tup: i64,
    n_dead_tup: i64,
    dead_pct: Option<f64>,
    last_vacuum: Option<DateTime<Utc>>,
    last_analyze: Option<DateTime<Utc>>,
    seq_scan: i64,
    idx_scan: i64,
}

#[derive(Serialize)]
struct TablesExport<'a> {
    generated_at: DateTime<Utc>,
    profile: Option<&'a str>,
    exported_count: usize,
    tables: Vec<ExportedTable<'a>>,
}

pub fn render_tables_json(
    tables: &[TableStat],
    profile: Option<&str>,
    now: DateTime<Utc>,
) -> serde_json::Result<String> {
    let exported: Vec<ExportedTable<'_>> = tables
        .iter()
        .map(|t| ExportedTable {
            schemaname: &t.schemaname,
            relname: &t.relname,
            n_live_tup: t.n_live_tup,
            n_dead_tup: t.n_dead_tup,
            dead_pct: t.dead_pct(),
            last_vacuum: t.last_vacuum,
            last_analyze: t.last_analyze,
            seq_scan: t.seq_scan,
            idx_scan: t.idx_scan,
        })
        .collect();
    let export = TablesExport {
        generated_at: now,
        profile,
        exported_count: exported.len(),
        tables: exported,
    };
    serde_json::to_string_pretty(&export)
}

pub fn write_tables(
    tables: &[TableStat],
    profile: Option<&str>,
    now: DateTime<Utc>,
) -> std::io::Result<PathBuf> {
    let body = render_tables_json(tables, profile, now)
        .map_err(|e| std::io::Error::other(format!("serialise tables: {e}")))?;
    let dir = export_dir();
    std::fs::create_dir_all(&dir)?;
    let path = tables_export_path(profile, now);
    std::fs::write(&path, body)?;
    Ok(path)
}

#[derive(Serialize)]
struct ExportedReplica<'a> {
    pid: i32,
    application_name: Option<&'a str>,
    client_addr: Option<&'a str>,
    state: Option<&'a str>,
    sync_state: Option<&'a str>,
    replay_lag_secs: Option<f64>,
    sent_lsn: Option<&'a str>,
    replay_lsn: Option<&'a str>,
}

#[derive(Serialize)]
struct ReplicationExport<'a> {
    generated_at: DateTime<Utc>,
    profile: Option<&'a str>,
    exported_count: usize,
    replicas: Vec<ExportedReplica<'a>>,
}

pub fn render_replication_json(
    replicas: &[Replica],
    profile: Option<&str>,
    now: DateTime<Utc>,
) -> serde_json::Result<String> {
    let exported: Vec<ExportedReplica<'_>> = replicas
        .iter()
        .map(|r| ExportedReplica {
            pid: r.pid,
            application_name: r.application_name.as_deref(),
            client_addr: r.client_addr.as_deref(),
            state: r.state.as_deref(),
            sync_state: r.sync_state.as_deref(),
            replay_lag_secs: r.replay_lag_secs,
            sent_lsn: r.sent_lsn.as_deref(),
            replay_lsn: r.replay_lsn.as_deref(),
        })
        .collect();
    let export = ReplicationExport {
        generated_at: now,
        profile,
        exported_count: exported.len(),
        replicas: exported,
    };
    serde_json::to_string_pretty(&export)
}

pub fn write_replication(
    replicas: &[Replica],
    profile: Option<&str>,
    now: DateTime<Utc>,
) -> std::io::Result<PathBuf> {
    let body = render_replication_json(replicas, profile, now)
        .map_err(|e| std::io::Error::other(format!("serialise replication: {e}")))?;
    let dir = export_dir();
    std::fs::create_dir_all(&dir)?;
    let path = replication_export_path(profile, now);
    std::fs::write(&path, body)?;
    Ok(path)
}

#[derive(Serialize)]
struct ExportedWait<'a> {
    wait_event_type: &'a str,
    wait_event: &'a str,
    count: u32,
    /// Share of all waiters in this snapshot. The Top Queries export
    /// has the same shape — it's the first thing you want to know.
    share_pct: f64,
}

#[derive(Serialize)]
struct WaitsExport<'a> {
    generated_at: DateTime<Utc>,
    profile: Option<&'a str>,
    /// Sum of `count` across all rows — the number of backends in a
    /// non-trivial wait state at snapshot time.
    waiting_total: u32,
    exported_count: usize,
    waits: Vec<ExportedWait<'a>>,
}

pub fn render_waits_json(
    waits: &[WaitRow],
    profile: Option<&str>,
    now: DateTime<Utc>,
) -> serde_json::Result<String> {
    let waiting_total: u32 = waits.iter().map(|w| w.count).sum();
    let total_f = waiting_total as f64;
    let exported: Vec<ExportedWait<'_>> = waits
        .iter()
        .map(|w| ExportedWait {
            wait_event_type: &w.wait_event_type,
            wait_event: &w.wait_event,
            count: w.count,
            share_pct: if total_f > 0.0 {
                100.0 * w.count as f64 / total_f
            } else {
                0.0
            },
        })
        .collect();
    let export = WaitsExport {
        generated_at: now,
        profile,
        waiting_total,
        exported_count: exported.len(),
        waits: exported,
    };
    serde_json::to_string_pretty(&export)
}

pub fn write_waits(
    waits: &[WaitRow],
    profile: Option<&str>,
    now: DateTime<Utc>,
) -> std::io::Result<PathBuf> {
    let body = render_waits_json(waits, profile, now)
        .map_err(|e| std::io::Error::other(format!("serialise waits: {e}")))?;
    let dir = export_dir();
    std::fs::create_dir_all(&dir)?;
    let path = waits_export_path(profile, now);
    std::fs::write(&path, body)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn tq(query: &str, total_ms: f64, calls: i64) -> TopQuery {
        TopQuery {
            query: query.to_string(),
            calls,
            total_exec_time_ms: total_ms,
            mean_exec_time_ms: total_ms / calls as f64,
            rows: calls * 10,
        }
    }

    #[test]
    fn render_includes_share_of_total_time() {
        let now = Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap();
        let queries = vec![tq("SELECT 1", 600.0, 10), tq("SELECT 2", 400.0, 20)];
        let json = render_json(&queries, Some("prod"), now).unwrap();
        assert!(json.contains("\"share_of_total_time_pct\": 60.0"));
        assert!(json.contains("\"share_of_total_time_pct\": 40.0"));
        assert!(json.contains("\"profile\": \"prod\""));
        assert!(json.contains("\"rank\": 1"));
    }

    #[test]
    fn render_handles_zero_total() {
        let now = Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap();
        let queries = vec![tq("SELECT pg_sleep(0)", 0.0, 1)];
        let json = render_json(&queries, None, now).unwrap();
        assert!(json.contains("\"share_of_total_time_pct\": 0.0"));
        assert!(json.contains("\"profile\": null"));
    }

    #[test]
    fn export_path_includes_profile_and_timestamp() {
        let now = Utc.with_ymd_and_hms(2026, 5, 12, 10, 34, 5).unwrap();
        let p = export_path(Some("prod"), now);
        assert!(
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .contains("top-queries-prod-20260512-103405.json")
        );
    }

    #[test]
    fn export_path_omits_profile_when_none() {
        let now = Utc.with_ymd_and_hms(2026, 5, 12, 10, 34, 5).unwrap();
        let p = export_path(None, now);
        assert_eq!(
            p.file_name().unwrap().to_string_lossy(),
            "top-queries-20260512-103405.json"
        );
    }

    #[test]
    fn activity_path_uses_distinct_prefix() {
        let now = Utc.with_ymd_and_hms(2026, 5, 12, 10, 34, 5).unwrap();
        let p = activity_export_path(Some("prod"), now);
        assert_eq!(
            p.file_name().unwrap().to_string_lossy(),
            "activity-prod-20260512-103405.json"
        );
    }

    fn empty_backend(pid: i32) -> Backend {
        Backend {
            pid,
            datname: None,
            usename: None,
            application_name: None,
            client_addr: None,
            backend_start: None,
            xact_start: None,
            query_start: None,
            state_change: None,
            wait_event_type: None,
            wait_event: None,
            state: None,
            backend_xid: None,
            backend_xmin: None,
            query: None,
            backend_type: None,
        }
    }

    #[test]
    fn activity_export_derives_duration_and_idle() {
        let now = Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap();
        let mut active = empty_backend(101);
        active.state = Some("active".into());
        active.query_start = Some(now - chrono::Duration::seconds(7));
        active.state_change = Some(now - chrono::Duration::seconds(7));

        let mut idle = empty_backend(202);
        idle.state = Some("idle in transaction".into());
        idle.query_start = Some(now - chrono::Duration::seconds(30));
        idle.state_change = Some(now - chrono::Duration::seconds(120));

        let backends = vec![&active, &idle];
        let json = render_activity_json(&backends, 2, Some("prod"), None, now).unwrap();
        assert!(json.contains("\"pid\": 101"));
        assert!(json.contains("\"duration_secs\": 7"));
        // Active backend: idle_secs stays null even with state_change set.
        assert!(json.contains("\"idle_secs\": null"));
        // Idle-in-transaction: 120s since state_change.
        assert!(json.contains("\"idle_secs\": 120"));
        assert!(json.contains("\"exported_count\": 2"));
        assert!(json.contains("\"total_count\": 2"));
        assert!(json.contains("\"filter\": null"));
    }

    #[test]
    fn activity_export_reports_filter_and_partial_count() {
        let now = Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap();
        let b = empty_backend(1);
        let backends = vec![&b];
        let json = render_activity_json(&backends, 50, None, Some("write"), now).unwrap();
        assert!(json.contains("\"filter\": \"write\""));
        assert!(json.contains("\"exported_count\": 1"));
        assert!(json.contains("\"total_count\": 50"));
    }

    #[test]
    fn activity_export_marks_self_connection() {
        let now = Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap();
        let mut b = empty_backend(7);
        b.application_name = Some("pgtop".into());
        let backends = vec![&b];
        let json = render_activity_json(&backends, 1, None, None, now).unwrap();
        assert!(json.contains("\"is_self\": true"));
    }

    #[test]
    fn locks_path_uses_distinct_prefix() {
        let now = Utc.with_ymd_and_hms(2026, 5, 12, 10, 34, 5).unwrap();
        let p = locks_export_path(Some("prod"), now);
        assert_eq!(
            p.file_name().unwrap().to_string_lossy(),
            "locks-prod-20260512-103405.json"
        );
    }

    fn lock(pid: i32, granted: bool, object: Option<&str>) -> Lock {
        Lock {
            pid,
            locktype: "relation".to_string(),
            mode: if granted {
                "AccessExclusiveLock".to_string()
            } else {
                "AccessShareLock".to_string()
            },
            granted,
            object: object.map(str::to_string),
        }
    }

    #[test]
    fn locks_export_counts_granted_and_waiting() {
        let now = Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap();
        let locks = vec![
            lock(101, true, Some("public.orders")),
            lock(102, false, Some("public.orders")),
            lock(103, false, Some("public.users")),
        ];
        let json = render_locks_json(&locks, Some("prod"), now).unwrap();
        assert!(json.contains("\"exported_count\": 3"));
        assert!(json.contains("\"granted_count\": 1"));
        assert!(json.contains("\"waiting_count\": 2"));
        assert!(json.contains("\"pid\": 101"));
        assert!(json.contains("\"object\": \"public.orders\""));
    }

    #[test]
    fn locks_export_handles_empty_object() {
        let now = Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap();
        let locks = vec![Lock {
            pid: 1,
            locktype: "transactionid".to_string(),
            mode: "ExclusiveLock".to_string(),
            granted: true,
            object: None,
        }];
        let json = render_locks_json(&locks, None, now).unwrap();
        assert!(json.contains("\"object\": null"));
        assert!(json.contains("\"locktype\": \"transactionid\""));
    }

    #[test]
    fn databases_export_materialises_cache_hit_pct() {
        let now = Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap();
        let dbs = vec![DatabaseStat {
            datname: "app".into(),
            numbackends: 5,
            xact_commit: 1000,
            xact_rollback: 10,
            blks_hit: 99,
            blks_read: 1,
            temp_bytes: 0,
            deadlocks: 0,
            tps: Some(42.0),
        }];
        let json = render_databases_json(&dbs, Some("prod"), now).unwrap();
        assert!(json.contains("\"cache_hit_pct\": 99.0"));
        assert!(json.contains("\"tps\": 42.0"));
        assert!(json.contains("\"exported_count\": 1"));
    }

    #[test]
    fn tables_export_includes_dead_pct() {
        let now = Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap();
        let tables = vec![TableStat {
            schemaname: "public".into(),
            relname: "orders".into(),
            n_live_tup: 80,
            n_dead_tup: 20,
            last_vacuum: None,
            last_analyze: None,
            seq_scan: 1,
            idx_scan: 100,
        }];
        let json = render_tables_json(&tables, None, now).unwrap();
        assert!(json.contains("\"dead_pct\": 20.0"));
        assert!(json.contains("\"relname\": \"orders\""));
    }

    #[test]
    fn waits_export_computes_share() {
        let now = Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap();
        let waits = vec![
            WaitRow {
                wait_event_type: "Lock".into(),
                wait_event: "relation".into(),
                count: 30,
            },
            WaitRow {
                wait_event_type: "Client".into(),
                wait_event: "ClientRead".into(),
                count: 10,
            },
        ];
        let json = render_waits_json(&waits, Some("prod"), now).unwrap();
        assert!(json.contains("\"share_pct\": 75.0"));
        assert!(json.contains("\"share_pct\": 25.0"));
        assert!(json.contains("\"waiting_total\": 40"));
    }

    #[test]
    fn waits_export_handles_zero_waiters() {
        let now = Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap();
        let json = render_waits_json(&[], None, now).unwrap();
        assert!(json.contains("\"waiting_total\": 0"));
        assert!(json.contains("\"exported_count\": 0"));
    }

    #[test]
    fn replication_export_handles_empty() {
        let now = Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap();
        let json = render_replication_json(&[], None, now).unwrap();
        assert!(json.contains("\"exported_count\": 0"));
    }
}
