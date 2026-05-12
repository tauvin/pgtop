//! Structural diff between two session snapshots.
//!
//! Each tab is diffed in its own model with its own identity keys
//! (`pid` for backends and replicas, `query` text for top queries, etc.).
//! Output is either a human-readable text report (default, written to
//! stdout) or a JSON document with the same structure (under `--json`,
//! for piping into other tools).

use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::replay::{
    LoadedBackend, LoadedDatabase, LoadedLock, LoadedReplica, LoadedTable, LoadedTopQueries,
    LoadedTopQuery, LoadedWait, SessionFile, load_session_file,
};

/// Entry point for `pgtop diff <A> <B>`.
pub fn run(a_path: &Path, b_path: &Path, json: bool) -> Result<(), String> {
    let a = load_session_file(a_path)?;
    let b = load_session_file(b_path)?;
    let diff = compute(&a, &b);
    if json {
        let body =
            serde_json::to_string_pretty(&diff).map_err(|e| format!("serialise diff: {e}"))?;
        println!("{body}");
    } else {
        print_human(&diff, &a, &b, a_path, b_path);
    }
    Ok(())
}

// ----- top-level diff types -----

#[derive(Serialize)]
pub struct SessionDiff {
    pub profile_a: Option<String>,
    pub profile_b: Option<String>,
    pub generated_at_a: DateTime<Utc>,
    pub generated_at_b: DateTime<Utc>,
    pub elapsed_secs: i64,
    pub activity: ActivityDiff,
    pub locks: LocksDiff,
    pub top_queries: TopQueriesDiff,
    pub replication: ReplicationDiff,
    pub databases: DatabasesDiff,
    pub tables: TablesDiff,
    pub waits: WaitsDiff,
}

#[derive(Serialize)]
pub struct ActivityDiff {
    pub count_before: usize,
    pub count_after: usize,
    pub added: Vec<BackendSummary>,
    pub removed: Vec<BackendSummary>,
    pub changed: Vec<BackendChange>,
}

#[derive(Serialize)]
pub struct BackendSummary {
    pub pid: i32,
    pub usename: Option<String>,
    pub state: Option<String>,
    pub datname: Option<String>,
    pub query: Option<String>,
}

#[derive(Serialize)]
pub struct BackendChange {
    pub pid: i32,
    pub state_before: Option<String>,
    pub state_after: Option<String>,
    pub query_before: Option<String>,
    pub query_after: Option<String>,
}

#[derive(Serialize)]
pub struct LocksDiff {
    pub count_before: usize,
    pub count_after: usize,
    pub added: Vec<LockSummary>,
    pub removed: Vec<LockSummary>,
}

#[derive(Serialize)]
pub struct LockSummary {
    pub pid: i32,
    pub locktype: String,
    pub mode: String,
    pub granted: bool,
    pub object: Option<String>,
}

#[derive(Serialize)]
pub struct TopQueriesDiff {
    pub state_before: &'static str,
    pub state_after: &'static str,
    pub added: Vec<TopQuerySummary>,
    pub removed: Vec<TopQuerySummary>,
    pub changed: Vec<TopQueryChange>,
}

#[derive(Serialize)]
pub struct TopQuerySummary {
    pub query: String,
    pub rank: usize,
    pub calls: i64,
    pub total_exec_time_ms: f64,
}

#[derive(Serialize)]
pub struct TopQueryChange {
    pub query: String,
    pub rank_before: usize,
    pub rank_after: usize,
    pub calls_delta: i64,
    pub total_exec_time_ms_delta: f64,
    pub mean_exec_time_ms_before: f64,
    pub mean_exec_time_ms_after: f64,
}

#[derive(Serialize)]
pub struct ReplicationDiff {
    pub count_before: usize,
    pub count_after: usize,
    pub added: Vec<ReplicaSummary>,
    pub removed: Vec<ReplicaSummary>,
    pub changed: Vec<ReplicaChange>,
}

#[derive(Serialize)]
pub struct ReplicaSummary {
    pub pid: i32,
    pub application_name: Option<String>,
    pub state: Option<String>,
    pub replay_lag_secs: Option<f64>,
}

#[derive(Serialize)]
pub struct ReplicaChange {
    pub pid: i32,
    pub state_before: Option<String>,
    pub state_after: Option<String>,
    pub replay_lag_secs_before: Option<f64>,
    pub replay_lag_secs_after: Option<f64>,
}

#[derive(Serialize)]
pub struct DatabasesDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<DatabaseChange>,
}

#[derive(Serialize)]
pub struct DatabaseChange {
    pub datname: String,
    pub xact_commit_delta: i64,
    pub xact_rollback_delta: i64,
    pub deadlocks_delta: i64,
    pub tps_before: Option<f64>,
    pub tps_after: Option<f64>,
    pub cache_hit_pct_before: f64,
    pub cache_hit_pct_after: f64,
    pub numbackends_before: i32,
    pub numbackends_after: i32,
}

#[derive(Serialize)]
pub struct TablesDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<TableChange>,
}

#[derive(Serialize)]
pub struct TableChange {
    pub schemaname: String,
    pub relname: String,
    pub n_dead_tup_delta: i64,
    pub n_live_tup_delta: i64,
    pub seq_scan_delta: i64,
    pub idx_scan_delta: i64,
    pub last_vacuum_changed: bool,
    pub last_analyze_changed: bool,
}

#[derive(Serialize)]
pub struct WaitsDiff {
    pub added: Vec<WaitSummary>,
    pub removed: Vec<WaitSummary>,
    pub changed: Vec<WaitChange>,
}

#[derive(Serialize)]
pub struct WaitSummary {
    pub wait_event_type: String,
    pub wait_event: String,
    pub count: u32,
}

#[derive(Serialize)]
pub struct WaitChange {
    pub wait_event_type: String,
    pub wait_event: String,
    pub count_before: u32,
    pub count_after: u32,
}

// ----- compute -----

pub fn compute(a: &SessionFile, b: &SessionFile) -> SessionDiff {
    let elapsed_secs = (b.generated_at - a.generated_at).num_seconds();
    SessionDiff {
        profile_a: a.profile.clone(),
        profile_b: b.profile.clone(),
        generated_at_a: a.generated_at,
        generated_at_b: b.generated_at,
        elapsed_secs,
        activity: diff_activity(&a.activity.backends, &b.activity.backends),
        locks: diff_locks(&a.locks, &b.locks),
        top_queries: diff_top_queries(&a.top_queries, &b.top_queries),
        replication: diff_replication(&a.replication, &b.replication),
        databases: diff_databases(&a.databases, &b.databases),
        tables: diff_tables(&a.tables, &b.tables),
        waits: diff_waits(&a.waits, &b.waits),
    }
}

fn diff_activity(a: &[LoadedBackend], b: &[LoadedBackend]) -> ActivityDiff {
    let a_map: HashMap<i32, &LoadedBackend> = a.iter().map(|x| (x.pid, x)).collect();
    let b_map: HashMap<i32, &LoadedBackend> = b.iter().map(|x| (x.pid, x)).collect();

    let added: Vec<BackendSummary> = b
        .iter()
        .filter(|x| !a_map.contains_key(&x.pid))
        .map(backend_summary)
        .collect();
    let removed: Vec<BackendSummary> = a
        .iter()
        .filter(|x| !b_map.contains_key(&x.pid))
        .map(backend_summary)
        .collect();
    let mut changed: Vec<BackendChange> = a
        .iter()
        .filter_map(|before| {
            let after = b_map.get(&before.pid)?;
            if before.state != after.state || before.query != after.query {
                Some(BackendChange {
                    pid: before.pid,
                    state_before: before.state.clone(),
                    state_after: after.state.clone(),
                    query_before: before.query.clone(),
                    query_after: after.query.clone(),
                })
            } else {
                None
            }
        })
        .collect();
    changed.sort_by_key(|c| c.pid);

    ActivityDiff {
        count_before: a.len(),
        count_after: b.len(),
        added,
        removed,
        changed,
    }
}

fn backend_summary(b: &LoadedBackend) -> BackendSummary {
    BackendSummary {
        pid: b.pid,
        usename: b.usename.clone(),
        state: b.state.clone(),
        datname: b.datname.clone(),
        query: b.query.clone(),
    }
}

fn lock_key(l: &LoadedLock) -> (i32, &str, &str, Option<&str>) {
    (
        l.pid,
        l.locktype.as_str(),
        l.mode.as_str(),
        l.object.as_deref(),
    )
}

fn diff_locks(a: &[LoadedLock], b: &[LoadedLock]) -> LocksDiff {
    let a_keys: std::collections::HashSet<_> = a.iter().map(lock_key).collect();
    let b_keys: std::collections::HashSet<_> = b.iter().map(lock_key).collect();

    let added: Vec<LockSummary> = b
        .iter()
        .filter(|l| !a_keys.contains(&lock_key(l)))
        .map(lock_summary)
        .collect();
    let removed: Vec<LockSummary> = a
        .iter()
        .filter(|l| !b_keys.contains(&lock_key(l)))
        .map(lock_summary)
        .collect();

    LocksDiff {
        count_before: a.len(),
        count_after: b.len(),
        added,
        removed,
    }
}

fn lock_summary(l: &LoadedLock) -> LockSummary {
    LockSummary {
        pid: l.pid,
        locktype: l.locktype.clone(),
        mode: l.mode.clone(),
        granted: l.granted,
        object: l.object.clone(),
    }
}

fn top_queries_state_label(t: &LoadedTopQueries) -> &'static str {
    match t {
        LoadedTopQueries::Loading => "loading",
        LoadedTopQueries::ExtensionMissing => "extension_missing",
        LoadedTopQueries::Available(_) => "available",
    }
}

fn diff_top_queries(a: &LoadedTopQueries, b: &LoadedTopQueries) -> TopQueriesDiff {
    let state_before = top_queries_state_label(a);
    let state_after = top_queries_state_label(b);
    let a_list: &[LoadedTopQuery] = match a {
        LoadedTopQueries::Available(qs) => qs,
        _ => &[],
    };
    let b_list: &[LoadedTopQuery] = match b {
        LoadedTopQueries::Available(qs) => qs,
        _ => &[],
    };

    let a_rank: HashMap<&str, (usize, &LoadedTopQuery)> = a_list
        .iter()
        .enumerate()
        .map(|(i, q)| (q.query.as_str(), (i + 1, q)))
        .collect();
    let b_rank: HashMap<&str, (usize, &LoadedTopQuery)> = b_list
        .iter()
        .enumerate()
        .map(|(i, q)| (q.query.as_str(), (i + 1, q)))
        .collect();

    let mut added: Vec<TopQuerySummary> = b_list
        .iter()
        .enumerate()
        .filter(|(_, q)| !a_rank.contains_key(q.query.as_str()))
        .map(|(i, q)| TopQuerySummary {
            query: q.query.clone(),
            rank: i + 1,
            calls: q.calls,
            total_exec_time_ms: q.total_exec_time_ms,
        })
        .collect();
    added.sort_by_key(|s| s.rank);

    let mut removed: Vec<TopQuerySummary> = a_list
        .iter()
        .enumerate()
        .filter(|(_, q)| !b_rank.contains_key(q.query.as_str()))
        .map(|(i, q)| TopQuerySummary {
            query: q.query.clone(),
            rank: i + 1,
            calls: q.calls,
            total_exec_time_ms: q.total_exec_time_ms,
        })
        .collect();
    removed.sort_by_key(|s| s.rank);

    let mut changed: Vec<TopQueryChange> = a_list
        .iter()
        .enumerate()
        .filter_map(|(i, before)| {
            let (after_rank, after) = b_rank.get(before.query.as_str())?;
            let calls_delta = after.calls - before.calls;
            let total_delta = after.total_exec_time_ms - before.total_exec_time_ms;
            let rank_changed = *after_rank != i + 1;
            // Heuristic: report a change if calls moved at all, the total
            // time shifted by more than 1ms, or the rank changed (likely
            // accompanied by either of the above, but rank flips alone
            // are interesting too).
            if calls_delta != 0 || total_delta.abs() > 1.0 || rank_changed {
                Some(TopQueryChange {
                    query: before.query.clone(),
                    rank_before: i + 1,
                    rank_after: *after_rank,
                    calls_delta,
                    total_exec_time_ms_delta: total_delta,
                    mean_exec_time_ms_before: before.mean_exec_time_ms,
                    mean_exec_time_ms_after: after.mean_exec_time_ms,
                })
            } else {
                None
            }
        })
        .collect();
    // Sort by absolute total-time delta, biggest movers first.
    changed.sort_by(|x, y| {
        y.total_exec_time_ms_delta
            .abs()
            .partial_cmp(&x.total_exec_time_ms_delta.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    TopQueriesDiff {
        state_before,
        state_after,
        added,
        removed,
        changed,
    }
}

fn diff_replication(a: &[LoadedReplica], b: &[LoadedReplica]) -> ReplicationDiff {
    let a_map: HashMap<i32, &LoadedReplica> = a.iter().map(|x| (x.pid, x)).collect();
    let b_map: HashMap<i32, &LoadedReplica> = b.iter().map(|x| (x.pid, x)).collect();

    let added: Vec<ReplicaSummary> = b
        .iter()
        .filter(|x| !a_map.contains_key(&x.pid))
        .map(replica_summary)
        .collect();
    let removed: Vec<ReplicaSummary> = a
        .iter()
        .filter(|x| !b_map.contains_key(&x.pid))
        .map(replica_summary)
        .collect();
    let mut changed: Vec<ReplicaChange> = a
        .iter()
        .filter_map(|before| {
            let after = b_map.get(&before.pid)?;
            // Lag is a float; consider any non-trivial shift a change.
            let lag_changed = match (before.replay_lag_secs, after.replay_lag_secs) {
                (Some(x), Some(y)) => (x - y).abs() > 0.01,
                (None, None) => false,
                _ => true,
            };
            if before.state != after.state || lag_changed {
                Some(ReplicaChange {
                    pid: before.pid,
                    state_before: before.state.clone(),
                    state_after: after.state.clone(),
                    replay_lag_secs_before: before.replay_lag_secs,
                    replay_lag_secs_after: after.replay_lag_secs,
                })
            } else {
                None
            }
        })
        .collect();
    changed.sort_by_key(|c| c.pid);

    ReplicationDiff {
        count_before: a.len(),
        count_after: b.len(),
        added,
        removed,
        changed,
    }
}

fn replica_summary(r: &LoadedReplica) -> ReplicaSummary {
    ReplicaSummary {
        pid: r.pid,
        application_name: r.application_name.clone(),
        state: r.state.clone(),
        replay_lag_secs: r.replay_lag_secs,
    }
}

fn diff_databases(a: &[LoadedDatabase], b: &[LoadedDatabase]) -> DatabasesDiff {
    let a_map: HashMap<&str, &LoadedDatabase> = a.iter().map(|d| (d.datname.as_str(), d)).collect();
    let b_map: HashMap<&str, &LoadedDatabase> = b.iter().map(|d| (d.datname.as_str(), d)).collect();

    let added: Vec<String> = b
        .iter()
        .filter(|d| !a_map.contains_key(d.datname.as_str()))
        .map(|d| d.datname.clone())
        .collect();
    let removed: Vec<String> = a
        .iter()
        .filter(|d| !b_map.contains_key(d.datname.as_str()))
        .map(|d| d.datname.clone())
        .collect();

    let mut changed: Vec<DatabaseChange> = a
        .iter()
        .filter_map(|before| {
            let after = b_map.get(before.datname.as_str())?;
            let xact_commit_delta = after.xact_commit - before.xact_commit;
            let xact_rollback_delta = after.xact_rollback - before.xact_rollback;
            let deadlocks_delta = after.deadlocks - before.deadlocks;
            let cache_a = cache_hit_pct(before);
            let cache_b = cache_hit_pct(after);
            let backends_changed = before.numbackends != after.numbackends;
            let cache_changed = (cache_a - cache_b).abs() > 0.05;
            let tps_changed = match (before.tps, after.tps) {
                (Some(x), Some(y)) => (x - y).abs() > 0.05,
                (None, None) => false,
                _ => true,
            };
            if xact_commit_delta != 0
                || xact_rollback_delta != 0
                || deadlocks_delta != 0
                || backends_changed
                || cache_changed
                || tps_changed
            {
                Some(DatabaseChange {
                    datname: before.datname.clone(),
                    xact_commit_delta,
                    xact_rollback_delta,
                    deadlocks_delta,
                    tps_before: before.tps,
                    tps_after: after.tps,
                    cache_hit_pct_before: cache_a,
                    cache_hit_pct_after: cache_b,
                    numbackends_before: before.numbackends,
                    numbackends_after: after.numbackends,
                })
            } else {
                None
            }
        })
        .collect();
    changed.sort_by_key(|c| std::cmp::Reverse(c.xact_commit_delta));

    DatabasesDiff {
        added,
        removed,
        changed,
    }
}

fn cache_hit_pct(d: &LoadedDatabase) -> f64 {
    let total = d.blks_hit + d.blks_read;
    if total > 0 {
        100.0 * d.blks_hit as f64 / total as f64
    } else {
        100.0
    }
}

fn diff_tables(a: &[LoadedTable], b: &[LoadedTable]) -> TablesDiff {
    type Key = (String, String);
    fn key(t: &LoadedTable) -> Key {
        (t.schemaname.clone(), t.relname.clone())
    }

    let a_map: HashMap<Key, &LoadedTable> = a.iter().map(|t| (key(t), t)).collect();
    let b_map: HashMap<Key, &LoadedTable> = b.iter().map(|t| (key(t), t)).collect();

    let added: Vec<String> = b
        .iter()
        .filter(|t| !a_map.contains_key(&key(t)))
        .map(|t| format!("{}.{}", t.schemaname, t.relname))
        .collect();
    let removed: Vec<String> = a
        .iter()
        .filter(|t| !b_map.contains_key(&key(t)))
        .map(|t| format!("{}.{}", t.schemaname, t.relname))
        .collect();

    let mut changed: Vec<TableChange> = a
        .iter()
        .filter_map(|before| {
            let after = b_map.get(&key(before))?;
            let dead_delta = after.n_dead_tup - before.n_dead_tup;
            let live_delta = after.n_live_tup - before.n_live_tup;
            let seq_delta = after.seq_scan - before.seq_scan;
            let idx_delta = after.idx_scan - before.idx_scan;
            let vac_changed = before.last_vacuum != after.last_vacuum;
            let ana_changed = before.last_analyze != after.last_analyze;
            if dead_delta != 0
                || live_delta != 0
                || seq_delta != 0
                || idx_delta != 0
                || vac_changed
                || ana_changed
            {
                Some(TableChange {
                    schemaname: before.schemaname.clone(),
                    relname: before.relname.clone(),
                    n_dead_tup_delta: dead_delta,
                    n_live_tup_delta: live_delta,
                    seq_scan_delta: seq_delta,
                    idx_scan_delta: idx_delta,
                    last_vacuum_changed: vac_changed,
                    last_analyze_changed: ana_changed,
                })
            } else {
                None
            }
        })
        .collect();
    changed.sort_by_key(|c| std::cmp::Reverse(c.n_dead_tup_delta.abs()));

    TablesDiff {
        added,
        removed,
        changed,
    }
}

fn diff_waits(a: &[LoadedWait], b: &[LoadedWait]) -> WaitsDiff {
    type Key = (String, String);
    fn key(w: &LoadedWait) -> Key {
        (w.wait_event_type.clone(), w.wait_event.clone())
    }

    let a_map: HashMap<Key, &LoadedWait> = a.iter().map(|w| (key(w), w)).collect();
    let b_map: HashMap<Key, &LoadedWait> = b.iter().map(|w| (key(w), w)).collect();

    let added: Vec<WaitSummary> = b
        .iter()
        .filter(|w| !a_map.contains_key(&key(w)))
        .map(wait_summary)
        .collect();
    let removed: Vec<WaitSummary> = a
        .iter()
        .filter(|w| !b_map.contains_key(&key(w)))
        .map(wait_summary)
        .collect();
    let mut changed: Vec<WaitChange> = a
        .iter()
        .filter_map(|before| {
            let after = b_map.get(&key(before))?;
            if before.count != after.count {
                Some(WaitChange {
                    wait_event_type: before.wait_event_type.clone(),
                    wait_event: before.wait_event.clone(),
                    count_before: before.count,
                    count_after: after.count,
                })
            } else {
                None
            }
        })
        .collect();
    changed.sort_by(|x, y| {
        let xd = (x.count_after as i64 - x.count_before as i64).abs();
        let yd = (y.count_after as i64 - y.count_before as i64).abs();
        yd.cmp(&xd)
    });

    WaitsDiff {
        added,
        removed,
        changed,
    }
}

fn wait_summary(w: &LoadedWait) -> WaitSummary {
    WaitSummary {
        wait_event_type: w.wait_event_type.clone(),
        wait_event: w.wait_event.clone(),
        count: w.count,
    }
}

// ----- human formatter -----

fn print_human(diff: &SessionDiff, a: &SessionFile, b: &SessionFile, a_path: &Path, b_path: &Path) {
    println!("pgtop diff");
    if diff.profile_a == diff.profile_b {
        if let Some(p) = &diff.profile_a {
            println!("  profile: {p}");
        }
    } else {
        println!(
            "  ! profile mismatch: A={:?}, B={:?}",
            diff.profile_a, diff.profile_b
        );
    }
    println!("  A: {} @ {}", a_path.display(), a.generated_at);
    println!("  B: {} @ {}", b_path.display(), b.generated_at);
    println!(
        "  elapsed: {} ({}s)",
        format_elapsed(diff.elapsed_secs),
        diff.elapsed_secs
    );
    println!();

    print_activity_section(&diff.activity);
    print_locks_section(&diff.locks);
    print_top_queries_section(&diff.top_queries);
    print_replication_section(&diff.replication);
    print_databases_section(&diff.databases);
    print_tables_section(&diff.tables);
    print_waits_section(&diff.waits);
}

fn format_elapsed(secs: i64) -> String {
    let s = secs.abs();
    let sign = if secs < 0 { "-" } else { "" };
    if s < 60 {
        format!("{sign}{s}s")
    } else if s < 3600 {
        format!("{sign}{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{sign}{}h{:02}m", s / 3600, (s % 3600) / 60)
    }
}

fn print_activity_section(d: &ActivityDiff) {
    println!(
        "Activity ({} -> {}, Δ {:+})",
        d.count_before,
        d.count_after,
        d.count_after as i64 - d.count_before as i64
    );
    if d.added.is_empty() && d.removed.is_empty() && d.changed.is_empty() {
        println!("  (no changes)");
        println!();
        return;
    }
    for s in &d.added {
        println!(
            "  + pid {:<7} {:<14} {}",
            s.pid,
            s.usename.as_deref().unwrap_or("—"),
            describe_backend(s)
        );
    }
    for s in &d.removed {
        println!(
            "  - pid {:<7} {:<14} {}",
            s.pid,
            s.usename.as_deref().unwrap_or("—"),
            describe_backend(s)
        );
    }
    for c in &d.changed {
        let state_line = describe_state_change(&c.state_before, &c.state_after);
        println!("  ~ pid {:<7} {state_line}", c.pid);
        if c.query_before != c.query_after {
            println!(
                "        query: {}",
                short_query_change(&c.query_before, &c.query_after)
            );
        }
    }
    println!();
}

fn describe_backend(s: &BackendSummary) -> String {
    let state = s.state.as_deref().unwrap_or("—");
    let q = s.query.as_deref().map(truncate_query).unwrap_or_default();
    if q.is_empty() {
        state.to_string()
    } else {
        format!("{state}  {q}")
    }
}

fn describe_state_change(before: &Option<String>, after: &Option<String>) -> String {
    let before = before.as_deref().unwrap_or("—");
    let after = after.as_deref().unwrap_or("—");
    if before == after {
        before.to_string()
    } else {
        format!("{before} → {after}")
    }
}

fn short_query_change(before: &Option<String>, after: &Option<String>) -> String {
    let before = before.as_deref().map(truncate_query).unwrap_or_default();
    let after = after.as_deref().map(truncate_query).unwrap_or_default();
    if before.is_empty() {
        format!("→ {after}")
    } else if after.is_empty() {
        format!("{before} →")
    } else {
        format!("{before} → {after}")
    }
}

fn truncate_query(q: &str) -> String {
    let one_line: String = q.split_whitespace().collect::<Vec<_>>().join(" ");
    match one_line.char_indices().nth(60) {
        Some((cutoff, _)) => format!("{}…", &one_line[..cutoff]),
        None => one_line,
    }
}

fn print_locks_section(d: &LocksDiff) {
    println!(
        "Locks ({} -> {}, Δ {:+})",
        d.count_before,
        d.count_after,
        d.count_after as i64 - d.count_before as i64
    );
    if d.added.is_empty() && d.removed.is_empty() {
        println!("  (no changes)");
        println!();
        return;
    }
    for l in &d.added {
        println!("  + {}", lock_line(l));
    }
    for l in &d.removed {
        println!("  - {}", lock_line(l));
    }
    println!();
}

fn lock_line(l: &LockSummary) -> String {
    let status = if l.granted { "granted" } else { "waiting" };
    let obj = l.object.as_deref().unwrap_or("—");
    format!(
        "pid {:<7} {:<8} {:<22} {:<15} {}",
        l.pid, status, l.mode, l.locktype, obj
    )
}

fn print_top_queries_section(d: &TopQueriesDiff) {
    print!("Top Queries");
    if d.state_before != d.state_after {
        print!(" (state: {} → {})", d.state_before, d.state_after);
    } else if d.state_before != "available" {
        print!(" (state: {})", d.state_before);
    }
    println!();

    if d.added.is_empty() && d.removed.is_empty() && d.changed.is_empty() {
        println!("  (no changes)");
        println!();
        return;
    }

    for s in &d.added {
        println!(
            "  + #{:<3} {}  (calls {}, total {:.1}ms)",
            s.rank,
            truncate_query(&s.query),
            s.calls,
            s.total_exec_time_ms
        );
    }
    for s in &d.removed {
        println!("  - #{:<3} {}", s.rank, truncate_query(&s.query));
    }
    for c in &d.changed {
        let rank_part = if c.rank_before == c.rank_after {
            format!("#{:<3}", c.rank_before)
        } else {
            format!("#{}→#{}", c.rank_before, c.rank_after)
        };
        println!(
            "  ~ {} {}  calls {:+}  total {:+.1}ms",
            rank_part,
            truncate_query(&c.query),
            c.calls_delta,
            c.total_exec_time_ms_delta
        );
    }
    println!();
}

fn print_replication_section(d: &ReplicationDiff) {
    if d.count_before == 0 && d.count_after == 0 {
        return;
    }
    println!("Replication ({} -> {})", d.count_before, d.count_after);
    if d.added.is_empty() && d.removed.is_empty() && d.changed.is_empty() {
        println!("  (no changes)");
        println!();
        return;
    }
    for r in &d.added {
        println!(
            "  + pid {:<7} {:<18} lag {}",
            r.pid,
            r.application_name.as_deref().unwrap_or("—"),
            format_lag(r.replay_lag_secs)
        );
    }
    for r in &d.removed {
        println!(
            "  - pid {:<7} {:<18} (gone)",
            r.pid,
            r.application_name.as_deref().unwrap_or("—")
        );
    }
    for c in &d.changed {
        println!(
            "  ~ pid {:<7} lag {} → {}",
            c.pid,
            format_lag(c.replay_lag_secs_before),
            format_lag(c.replay_lag_secs_after)
        );
    }
    println!();
}

fn format_lag(secs: Option<f64>) -> String {
    match secs {
        Some(s) => format!("{s:.1}s"),
        None => "—".to_string(),
    }
}

fn print_databases_section(d: &DatabasesDiff) {
    if d.added.is_empty() && d.removed.is_empty() && d.changed.is_empty() {
        return;
    }
    println!("Databases");
    for name in &d.added {
        println!("  + {name}");
    }
    for name in &d.removed {
        println!("  - {name}");
    }
    for c in &d.changed {
        let mut parts: Vec<String> = vec![];
        if c.xact_commit_delta != 0 {
            parts.push(format!("commits {:+}", c.xact_commit_delta));
        }
        if c.xact_rollback_delta != 0 {
            parts.push(format!("rollbacks {:+}", c.xact_rollback_delta));
        }
        if c.deadlocks_delta != 0 {
            parts.push(format!("deadlocks {:+}", c.deadlocks_delta));
        }
        if c.tps_before != c.tps_after {
            parts.push(format!(
                "tps {} → {}",
                fmt_opt_f64(c.tps_before),
                fmt_opt_f64(c.tps_after)
            ));
        }
        if (c.cache_hit_pct_after - c.cache_hit_pct_before).abs() > 0.05 {
            parts.push(format!(
                "cache_hit {:.1}% → {:.1}%",
                c.cache_hit_pct_before, c.cache_hit_pct_after
            ));
        }
        if c.numbackends_before != c.numbackends_after {
            parts.push(format!(
                "conns {} → {}",
                c.numbackends_before, c.numbackends_after
            ));
        }
        println!("  ~ {}: {}", c.datname, parts.join("  "));
    }
    println!();
}

fn fmt_opt_f64(v: Option<f64>) -> String {
    match v {
        Some(x) => format!("{x:.1}"),
        None => "—".to_string(),
    }
}

fn print_tables_section(d: &TablesDiff) {
    if d.added.is_empty() && d.removed.is_empty() && d.changed.is_empty() {
        return;
    }
    println!("Tables");
    for n in &d.added {
        println!("  + {n}");
    }
    for n in &d.removed {
        println!("  - {n}");
    }
    for c in &d.changed {
        let mut parts: Vec<String> = vec![];
        if c.n_dead_tup_delta != 0 {
            parts.push(format!("dead {:+}", c.n_dead_tup_delta));
        }
        if c.n_live_tup_delta != 0 {
            parts.push(format!("live {:+}", c.n_live_tup_delta));
        }
        if c.seq_scan_delta != 0 {
            parts.push(format!("seq {:+}", c.seq_scan_delta));
        }
        if c.idx_scan_delta != 0 {
            parts.push(format!("idx {:+}", c.idx_scan_delta));
        }
        if c.last_vacuum_changed {
            parts.push("vacuumed".into());
        }
        if c.last_analyze_changed {
            parts.push("analyzed".into());
        }
        println!("  ~ {}.{}: {}", c.schemaname, c.relname, parts.join("  "));
    }
    println!();
}

fn print_waits_section(d: &WaitsDiff) {
    if d.added.is_empty() && d.removed.is_empty() && d.changed.is_empty() {
        return;
    }
    println!("Waits");
    for w in &d.added {
        println!(
            "  + {}/{} ({} waiters)",
            w.wait_event_type, w.wait_event, w.count
        );
    }
    for w in &d.removed {
        println!(
            "  - {}/{} (was {} waiters)",
            w.wait_event_type, w.wait_event, w.count
        );
    }
    for c in &d.changed {
        let delta = c.count_after as i64 - c.count_before as i64;
        println!(
            "  ~ {}/{}: {} → {} ({:+})",
            c.wait_event_type, c.wait_event, c.count_before, c.count_after, delta
        );
    }
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay::{LoadedTopQueries, SessionActivity};

    fn empty_session() -> SessionFile {
        SessionFile {
            schema_version: "0.2".into(),
            generated_at: Utc::now(),
            profile: None,
            current_tab: "activity".into(),
            filter: None,
            activity: SessionActivity {
                total_count: 0,
                exported_count: 0,
                backends: vec![],
            },
            locks: vec![],
            top_queries: LoadedTopQueries::Loading,
            replication: vec![],
            databases: vec![],
            tables: vec![],
            waits: vec![],
        }
    }

    fn backend(pid: i32, state: &str, query: &str) -> LoadedBackend {
        LoadedBackend {
            pid,
            datname: Some("app".into()),
            usename: Some("alice".into()),
            application_name: None,
            client_addr: None,
            state: Some(state.into()),
            wait_event_type: None,
            wait_event: None,
            backend_type: Some("client backend".into()),
            backend_start: None,
            xact_start: None,
            query_start: None,
            state_change: None,
            backend_xid: None,
            backend_xmin: None,
            query: Some(query.into()),
        }
    }

    #[test]
    fn activity_added_removed_changed() {
        let mut a = empty_session();
        let mut b = empty_session();
        a.activity.backends = vec![
            backend(101, "active", "SELECT 1"),
            backend(202, "idle", "SELECT 2"),
        ];
        b.activity.backends = vec![
            backend(101, "idle in transaction", "SELECT 1 changed"),
            backend(303, "active", "SELECT 3"),
        ];
        let diff = compute(&a, &b);
        assert_eq!(diff.activity.added.len(), 1);
        assert_eq!(diff.activity.added[0].pid, 303);
        assert_eq!(diff.activity.removed.len(), 1);
        assert_eq!(diff.activity.removed[0].pid, 202);
        assert_eq!(diff.activity.changed.len(), 1);
        assert_eq!(diff.activity.changed[0].pid, 101);
    }

    #[test]
    fn top_queries_calls_delta() {
        let mut a = empty_session();
        let mut b = empty_session();
        a.top_queries = LoadedTopQueries::Available(vec![LoadedTopQuery {
            query: "SELECT 1".into(),
            calls: 100,
            total_exec_time_ms: 500.0,
            mean_exec_time_ms: 5.0,
            rows: 100,
        }]);
        b.top_queries = LoadedTopQueries::Available(vec![LoadedTopQuery {
            query: "SELECT 1".into(),
            calls: 150,
            total_exec_time_ms: 800.0,
            mean_exec_time_ms: 5.3,
            rows: 150,
        }]);
        let diff = compute(&a, &b);
        assert_eq!(diff.top_queries.changed.len(), 1);
        assert_eq!(diff.top_queries.changed[0].calls_delta, 50);
        assert!((diff.top_queries.changed[0].total_exec_time_ms_delta - 300.0).abs() < 0.01);
    }

    #[test]
    fn databases_only_significant_changes() {
        let mut a = empty_session();
        let mut b = empty_session();
        let make = |commit: i64, deadlocks: i64| LoadedDatabase {
            datname: "app".into(),
            numbackends: 1,
            tps: Some(10.0),
            xact_commit: commit,
            xact_rollback: 0,
            blks_hit: 100,
            blks_read: 1,
            temp_bytes: 0,
            deadlocks,
        };
        a.databases = vec![make(1000, 0)];
        b.databases = vec![make(1240, 1)];
        let diff = compute(&a, &b);
        assert_eq!(diff.databases.changed.len(), 1);
        assert_eq!(diff.databases.changed[0].xact_commit_delta, 240);
        assert_eq!(diff.databases.changed[0].deadlocks_delta, 1);
    }

    #[test]
    fn waits_diff_by_event_pair() {
        let mut a = empty_session();
        let mut b = empty_session();
        a.waits = vec![
            LoadedWait {
                wait_event_type: "Lock".into(),
                wait_event: "relation".into(),
                count: 5,
            },
            LoadedWait {
                wait_event_type: "Client".into(),
                wait_event: "ClientRead".into(),
                count: 10,
            },
        ];
        b.waits = vec![
            LoadedWait {
                wait_event_type: "Lock".into(),
                wait_event: "relation".into(),
                count: 12,
            },
            LoadedWait {
                wait_event_type: "IO".into(),
                wait_event: "DataFileRead".into(),
                count: 3,
            },
        ];
        let diff = compute(&a, &b);
        assert_eq!(diff.waits.changed.len(), 1);
        assert_eq!(diff.waits.changed[0].count_before, 5);
        assert_eq!(diff.waits.changed[0].count_after, 12);
        assert_eq!(diff.waits.added.len(), 1);
        assert_eq!(diff.waits.removed.len(), 1);
    }

    #[test]
    fn elapsed_secs_negative_when_b_earlier() {
        let mut a = empty_session();
        let mut b = empty_session();
        a.generated_at = Utc::now();
        b.generated_at = a.generated_at - chrono::Duration::seconds(30);
        let diff = compute(&a, &b);
        assert_eq!(diff.elapsed_secs, -30);
    }
}
