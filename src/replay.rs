//! Read session JSON files produced by `write_session` and restore them
//! into the in-memory shapes the live TUI consumes.
//!
//! The on-disk schema is *not* a one-to-one mirror of the runtime types —
//! the export rewrites a few derived fields (duration_secs, dead_pct, …)
//! for the reader's convenience. Loading therefore goes through its own
//! `SessionFile` model and converts back, dropping the derived bits.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::app::{ConnectionState, ConnectionStatus, Tab, WaitRow};
use crate::db::{Backend, DatabaseStat, Lock, Replica, TableStat, TopQueriesSnapshot, TopQuery};

#[derive(Debug, Deserialize)]
pub struct SessionFile {
    pub schema_version: String,
    /// Timestamp at which the snapshot was taken. Not currently shown in
    /// replay (the title bar has space for it as a future enhancement);
    /// kept on the struct because `pgtop diff` will need it.
    #[allow(dead_code)]
    pub generated_at: DateTime<Utc>,
    pub profile: Option<String>,
    pub current_tab: String,
    pub filter: Option<String>,
    pub activity: SessionActivity,
    pub locks: Vec<LoadedLock>,
    pub top_queries: LoadedTopQueries,
    pub replication: Vec<LoadedReplica>,
    pub databases: Vec<LoadedDatabase>,
    pub tables: Vec<LoadedTable>,
    pub waits: Vec<LoadedWait>,
}

#[derive(Debug, Deserialize)]
pub struct SessionActivity {
    #[allow(dead_code)]
    pub total_count: usize,
    #[allow(dead_code)]
    pub exported_count: usize,
    pub backends: Vec<LoadedBackend>,
}

#[derive(Debug, Deserialize)]
pub struct LoadedBackend {
    pub pid: i32,
    pub datname: Option<String>,
    pub usename: Option<String>,
    pub application_name: Option<String>,
    pub client_addr: Option<String>,
    pub state: Option<String>,
    pub wait_event_type: Option<String>,
    pub wait_event: Option<String>,
    pub backend_type: Option<String>,
    pub backend_start: Option<DateTime<Utc>>,
    pub xact_start: Option<DateTime<Utc>>,
    pub query_start: Option<DateTime<Utc>>,
    pub state_change: Option<DateTime<Utc>>,
    pub backend_xid: Option<String>,
    pub backend_xmin: Option<String>,
    pub query: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LoadedLock {
    pub pid: i32,
    pub locktype: String,
    pub mode: String,
    pub granted: bool,
    pub object: Option<String>,
}

/// Tagged enum matching `SessionTopQueries` in the writer.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "queries")]
pub enum LoadedTopQueries {
    Loading,
    ExtensionMissing,
    Available(Vec<LoadedTopQuery>),
}

#[derive(Debug, Deserialize)]
pub struct LoadedTopQuery {
    pub query: String,
    pub calls: i64,
    pub total_exec_time_ms: f64,
    pub mean_exec_time_ms: f64,
    pub rows: i64,
}

#[derive(Debug, Deserialize)]
pub struct LoadedReplica {
    pub pid: i32,
    pub application_name: Option<String>,
    pub client_addr: Option<String>,
    pub state: Option<String>,
    pub sync_state: Option<String>,
    pub replay_lag_secs: Option<f64>,
    pub sent_lsn: Option<String>,
    pub replay_lsn: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LoadedDatabase {
    pub datname: String,
    pub numbackends: i32,
    pub tps: Option<f64>,
    pub xact_commit: i64,
    pub xact_rollback: i64,
    pub blks_hit: i64,
    pub blks_read: i64,
    pub temp_bytes: i64,
    pub deadlocks: i64,
}

#[derive(Debug, Deserialize)]
pub struct LoadedTable {
    pub schemaname: String,
    pub relname: String,
    pub n_live_tup: i64,
    pub n_dead_tup: i64,
    pub last_vacuum: Option<DateTime<Utc>>,
    pub last_analyze: Option<DateTime<Utc>>,
    pub seq_scan: i64,
    pub idx_scan: i64,
}

#[derive(Debug, Deserialize)]
pub struct LoadedWait {
    pub wait_event_type: String,
    pub wait_event: String,
    pub count: u32,
}

/// Parse a session file. The schema_version must match what this build
/// of pgtop knows how to load.
pub fn load_session_file(path: &Path) -> Result<SessionFile, String> {
    let body =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let file: SessionFile =
        serde_json::from_str(&body).map_err(|e| format!("parse {}: {e}", path.display()))?;
    if file.schema_version != crate::export::SESSION_SCHEMA_VERSION {
        return Err(format!(
            "schema mismatch: file is {}, this build expects {}. \
             Run a newer (or older) pgtop, or re-take the snapshot.",
            file.schema_version,
            crate::export::SESSION_SCHEMA_VERSION,
        ));
    }
    Ok(file)
}

/// Build a `ConnectionState` from a loaded session file. The connection
/// is marked `read_only` and uses a fake DSN (`replay://<file>`) so any
/// code that inspects it sees an obviously non-live source.
pub fn to_connection_state(file: &SessionFile, source_path: &Path) -> ConnectionState {
    let display_dsn = format!("replay://{}", source_path.display());
    let mut conn = ConnectionState::new(
        display_dsn,
        true,
        false,
        file.profile.clone(),
        std::time::Duration::from_secs(30),
    );
    conn.status = ConnectionStatus::Connected;
    conn.set_backends(file.activity.backends.iter().map(to_backend).collect());
    conn.set_locks(file.locks.iter().map(to_lock).collect());
    conn.set_top_queries(to_top_queries(&file.top_queries));
    conn.set_replication(file.replication.iter().map(to_replica).collect());
    conn.set_databases(file.databases.iter().map(to_database).collect());
    conn.set_tables(file.tables.iter().map(to_table).collect());

    // Waits is rebuilt from backends inside set_backends, but the export
    // can also be inspected directly; override with the recorded list
    // for fidelity to what the user was seeing.
    conn.waits = file.waits.iter().map(to_wait).collect();

    if let Some(pattern) = &file.filter
        && !pattern.is_empty()
    {
        conn.filter.input = pattern.as_str().into();
        conn.filter.rebuild_regex();
    }
    conn
}

/// Tab id stored in the file → enum. Falls back to Activity for files
/// from a future schema with a tab we don't know.
pub fn current_tab(file: &SessionFile) -> Tab {
    Tab::from_id(&file.current_tab).unwrap_or(Tab::Activity)
}

fn to_backend(b: &LoadedBackend) -> Backend {
    Backend {
        pid: b.pid,
        datname: b.datname.clone(),
        usename: b.usename.clone(),
        application_name: b.application_name.clone(),
        client_addr: b.client_addr.clone(),
        backend_start: b.backend_start,
        xact_start: b.xact_start,
        query_start: b.query_start,
        state_change: b.state_change,
        wait_event_type: b.wait_event_type.clone(),
        wait_event: b.wait_event.clone(),
        state: b.state.clone(),
        backend_xid: b.backend_xid.clone(),
        backend_xmin: b.backend_xmin.clone(),
        query: b.query.clone(),
        backend_type: b.backend_type.clone(),
    }
}

fn to_lock(l: &LoadedLock) -> Lock {
    Lock {
        pid: l.pid,
        locktype: l.locktype.clone(),
        mode: l.mode.clone(),
        granted: l.granted,
        object: l.object.clone(),
    }
}

fn to_top_queries(t: &LoadedTopQueries) -> TopQueriesSnapshot {
    match t {
        LoadedTopQueries::Loading => TopQueriesSnapshot::Loading,
        LoadedTopQueries::ExtensionMissing => TopQueriesSnapshot::ExtensionMissing,
        LoadedTopQueries::Available(qs) => TopQueriesSnapshot::Available(
            qs.iter()
                .map(|q| TopQuery {
                    query: q.query.clone(),
                    calls: q.calls,
                    total_exec_time_ms: q.total_exec_time_ms,
                    mean_exec_time_ms: q.mean_exec_time_ms,
                    rows: q.rows,
                })
                .collect(),
        ),
    }
}

fn to_replica(r: &LoadedReplica) -> Replica {
    Replica {
        pid: r.pid,
        application_name: r.application_name.clone(),
        client_addr: r.client_addr.clone(),
        state: r.state.clone(),
        sync_state: r.sync_state.clone(),
        replay_lag_secs: r.replay_lag_secs,
        sent_lsn: r.sent_lsn.clone(),
        replay_lsn: r.replay_lsn.clone(),
    }
}

fn to_database(d: &LoadedDatabase) -> DatabaseStat {
    DatabaseStat {
        datname: d.datname.clone(),
        numbackends: d.numbackends,
        xact_commit: d.xact_commit,
        xact_rollback: d.xact_rollback,
        blks_hit: d.blks_hit,
        blks_read: d.blks_read,
        temp_bytes: d.temp_bytes,
        deadlocks: d.deadlocks,
        tps: d.tps,
    }
}

fn to_table(t: &LoadedTable) -> TableStat {
    TableStat {
        schemaname: t.schemaname.clone(),
        relname: t.relname.clone(),
        n_live_tup: t.n_live_tup,
        n_dead_tup: t.n_dead_tup,
        last_vacuum: t.last_vacuum,
        last_analyze: t.last_analyze,
        seq_scan: t.seq_scan,
        idx_scan: t.idx_scan,
    }
}

fn to_wait(w: &LoadedWait) -> WaitRow {
    WaitRow {
        wait_event_type: w.wait_event_type.clone(),
        wait_event: w.wait_event.clone(),
        count: w.count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_session_through_write_and_load() {
        use crate::export::{SessionInputs, render_session_json};

        // Build a minimal session with one of each thing populated.
        let now = chrono::Utc::now();
        let mut backend = Backend {
            pid: 101,
            datname: Some("app".into()),
            usename: Some("alice".into()),
            application_name: Some("psql".into()),
            client_addr: None,
            backend_start: None,
            xact_start: None,
            query_start: Some(now - chrono::Duration::seconds(7)),
            state_change: Some(now - chrono::Duration::seconds(7)),
            wait_event_type: None,
            wait_event: None,
            state: Some("active".into()),
            backend_xid: None,
            backend_xmin: None,
            query: Some("SELECT 1".into()),
            backend_type: Some("client backend".into()),
        };
        let backends_all = vec![backend.clone()];
        let backends_filtered = vec![&backend];

        let locks = vec![Lock {
            pid: 101,
            locktype: "relation".into(),
            mode: "AccessShareLock".into(),
            granted: true,
            object: Some("public.t".into()),
        }];

        let top = TopQueriesSnapshot::Available(vec![TopQuery {
            query: "SELECT 1".into(),
            calls: 5,
            total_exec_time_ms: 100.0,
            mean_exec_time_ms: 20.0,
            rows: 5,
        }]);

        let inputs = SessionInputs {
            profile: Some("test"),
            current_tab: "locks",
            filter: Some("alice"),
            backends_all: &backends_all,
            backends_filtered: &backends_filtered,
            locks: &locks,
            top_queries: &top,
            replication: &[],
            databases: &[],
            tables: &[],
            waits: &[],
        };

        let json = render_session_json(&inputs, now).unwrap();
        let parsed: SessionFile = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.schema_version, crate::export::SESSION_SCHEMA_VERSION);
        assert_eq!(parsed.profile.as_deref(), Some("test"));
        assert_eq!(parsed.current_tab, "locks");
        assert_eq!(parsed.filter.as_deref(), Some("alice"));
        assert_eq!(parsed.activity.backends.len(), 1);
        assert_eq!(parsed.activity.backends[0].pid, 101);
        assert_eq!(parsed.locks.len(), 1);
        assert!(matches!(parsed.top_queries, LoadedTopQueries::Available(ref qs) if qs.len() == 1));

        // And the round-trip into ConnectionState preserves the data.
        let conn = to_connection_state(&parsed, Path::new("/tmp/x.json"));
        assert!(conn.read_only);
        assert!(!conn.actions_allowed);
        assert_eq!(conn.backends.len(), 1);
        assert_eq!(conn.backends[0].pid, 101);
        assert_eq!(conn.locks.len(), 1);
        // Filter applied — both backends should still match "alice" (the
        // usename), so filtered count == backends count.
        assert_eq!(conn.filtered.len(), 1);

        // Silence unused-mut warning on the prototype backend.
        backend.pid = 0;
        let _ = backend;
    }

    #[test]
    fn schema_mismatch_is_reported_clearly() {
        let wrong = r#"{
            "schema_version": "9.9",
            "generated_at": "2026-01-01T00:00:00Z",
            "profile": null,
            "current_tab": "activity",
            "filter": null,
            "activity": { "total_count": 0, "exported_count": 0, "backends": [] },
            "locks": [],
            "top_queries": { "state": "loading" },
            "replication": [],
            "databases": [],
            "tables": [],
            "waits": []
        }"#;
        let tmp = std::env::temp_dir().join("pgtop-replay-test-wrong.json");
        std::fs::write(&tmp, wrong).unwrap();
        let err = load_session_file(&tmp).unwrap_err();
        assert!(err.contains("schema mismatch"));
    }

    #[test]
    fn current_tab_recovers_from_unknown() {
        let mut file = SessionFile {
            schema_version: "0.2".into(),
            generated_at: chrono::Utc::now(),
            profile: None,
            current_tab: "nonsense".into(),
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
        };
        assert_eq!(current_tab(&file), Tab::Activity);

        file.current_tab = "locks".into();
        assert_eq!(current_tab(&file), Tab::Locks);
    }
}
