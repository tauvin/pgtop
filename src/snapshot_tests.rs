//! UI snapshot tests via `insta` + `ratatui::TestBackend`.
//!
//! Each test builds a fixed `App` with deterministic data, renders one
//! frame at a fixed `now`, and snapshots the terminal buffer as text.
//! Changes to layout, formatting, or colours surface as diffs.

use chrono::{DateTime, TimeZone, Utc};
use ratatui::{Terminal, backend::TestBackend};

use crate::app::{App, ConnectionState, Mode, Tab, WaitRow};
use crate::db::{Backend, DatabaseStat, Lock, Replica, TableStat, TopQueriesSnapshot, TopQuery};
use crate::ui;

const WIDTH: u16 = 120;
const HEIGHT: u16 = 24;

/// Fixed reference time used by all snapshots. Any duration in the UI is
/// computed relative to this so snapshots are stable.
fn fixed_now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap()
}

fn make_conn() -> ConnectionState {
    ConnectionState::new(
        "postgres://test".to_string(),
        false,
        false,
        Some("test".to_string()),
        std::time::Duration::from_secs(30),
    )
}

fn make_app(conn: ConnectionState) -> App {
    let mut app = App::new(vec![conn]);
    app.active_mut().status = crate::app::ConnectionStatus::Connected;
    app
}

fn backend(pid: i32) -> Backend {
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

fn render_snapshot(app: &mut App) -> String {
    let backend = TestBackend::new(WIDTH, HEIGHT);
    let mut terminal = Terminal::new(backend).unwrap();
    let now = fixed_now();
    terminal.draw(|frame| ui::render(frame, app, now)).unwrap();
    terminal.backend().to_string()
}

#[test]
fn activity_tab_with_three_backends() {
    let now = fixed_now();
    let mut conn = make_conn();

    let mut b1 = backend(101);
    b1.usename = Some("alice".to_string());
    b1.datname = Some("app".to_string());
    b1.state = Some("active".to_string());
    b1.query = Some("SELECT * FROM users WHERE id = 1".to_string());
    b1.query_start = Some(now - chrono::Duration::seconds(5));

    let mut b2 = backend(202);
    b2.usename = Some("bob".to_string());
    b2.datname = Some("app".to_string());
    b2.state = Some("idle in transaction".to_string());
    b2.query = Some("UPDATE accounts SET balance = balance - 1".to_string());
    b2.query_start = Some(now - chrono::Duration::seconds(120));

    let mut b3 = backend(303);
    b3.usename = Some("carol".to_string());
    b3.datname = Some("reports".to_string());
    b3.state = Some("active".to_string());
    b3.wait_event_type = Some("Lock".to_string());
    b3.wait_event = Some("relation".to_string());
    b3.query = Some("VACUUM ANALYZE big_table".to_string());
    b3.query_start = Some(now - chrono::Duration::seconds(45));

    conn.set_backends(vec![b1, b2, b3]);
    let mut app = make_app(conn);
    app.current_tab = Tab::Activity;

    insta::assert_snapshot!(render_snapshot(&mut app));
}

#[test]
fn activity_tab_empty() {
    let conn = make_conn();
    let mut app = make_app(conn);
    app.current_tab = Tab::Activity;

    insta::assert_snapshot!(render_snapshot(&mut app));
}

#[test]
fn locks_tab_with_blocking_pair() {
    let mut conn = make_conn();
    conn.set_locks(vec![
        Lock {
            pid: 1001,
            locktype: "relation".to_string(),
            mode: "AccessExclusiveLock".to_string(),
            granted: true,
            object: Some("public.orders".to_string()),
        },
        Lock {
            pid: 1002,
            locktype: "relation".to_string(),
            mode: "AccessShareLock".to_string(),
            granted: false,
            object: Some("public.orders".to_string()),
        },
    ]);
    let mut app = make_app(conn);
    app.current_tab = Tab::Locks;

    insta::assert_snapshot!(render_snapshot(&mut app));
}

#[test]
fn databases_tab() {
    let mut conn = make_conn();
    conn.set_databases(vec![
        DatabaseStat {
            datname: "app".to_string(),
            numbackends: 42,
            xact_commit: 1_234_567,
            xact_rollback: 89,
            blks_hit: 9_900_000,
            blks_read: 100_000,
            temp_bytes: 0,
            deadlocks: 0,
            tps: Some(125.4),
        },
        DatabaseStat {
            datname: "reports".to_string(),
            numbackends: 3,
            xact_commit: 4_321,
            xact_rollback: 12,
            blks_hit: 500_000,
            blks_read: 50_000,
            temp_bytes: 1_048_576,
            deadlocks: 2,
            tps: None,
        },
    ]);
    let mut app = make_app(conn);
    app.current_tab = Tab::Databases;

    insta::assert_snapshot!(render_snapshot(&mut app));
}

#[test]
fn tables_tab() {
    let now = fixed_now();
    let mut conn = make_conn();
    conn.set_tables(vec![
        TableStat {
            schemaname: "public".to_string(),
            relname: "orders".to_string(),
            n_live_tup: 1_500_000,
            n_dead_tup: 200_000,
            last_vacuum: Some(now - chrono::Duration::hours(2)),
            last_analyze: Some(now - chrono::Duration::minutes(30)),
            seq_scan: 42,
            idx_scan: 1_200_000,
        },
        TableStat {
            schemaname: "public".to_string(),
            relname: "users".to_string(),
            n_live_tup: 50_000,
            n_dead_tup: 100,
            last_vacuum: Some(now - chrono::Duration::days(3)),
            last_analyze: None,
            seq_scan: 5,
            idx_scan: 800_000,
        },
    ]);
    let mut app = make_app(conn);
    app.current_tab = Tab::Tables;

    insta::assert_snapshot!(render_snapshot(&mut app));
}

#[test]
fn detail_popup() {
    let now = fixed_now();
    let mut conn = make_conn();
    let mut b = backend(404);
    b.usename = Some("dave".to_string());
    b.datname = Some("app".to_string());
    b.state = Some("active".to_string());
    b.application_name = Some("psql".to_string());
    b.client_addr = Some("10.0.0.7".to_string());
    b.query =
        Some("SELECT count(*) FROM events WHERE created_at > now() - interval '1 day'".to_string());
    b.query_start = Some(now - chrono::Duration::seconds(7));
    conn.set_backends(vec![b]);
    let mut app = make_app(conn);
    app.current_tab = Tab::Activity;
    app.mode = Mode::Detail(404);

    insta::assert_snapshot!(render_snapshot(&mut app));
}

#[test]
fn confirm_terminate_popup_partial_typing() {
    let mut conn = make_conn();
    let mut b = backend(505);
    b.usename = Some("eve".to_string());
    b.state = Some("idle in transaction".to_string());
    b.query = Some("BEGIN".to_string());
    conn.set_backends(vec![b]);
    let mut app = make_app(conn);
    app.current_tab = Tab::Activity;
    app.mode = Mode::ConfirmTerminate(505, "ye".to_string());

    insta::assert_snapshot!(render_snapshot(&mut app));
}

#[test]
fn top_queries_tab_available() {
    let mut conn = make_conn();
    conn.set_top_queries(TopQueriesSnapshot::Available(vec![
        TopQuery {
            query: "SELECT * FROM orders WHERE created_at > now() - interval '1 day'".to_string(),
            calls: 12_345,
            total_exec_time_ms: 56_789.12,
            mean_exec_time_ms: 4.6,
            rows: 9_876_543,
        },
        TopQuery {
            query: "UPDATE accounts SET balance = balance - $1 WHERE id = $2".to_string(),
            calls: 4_321,
            total_exec_time_ms: 8_765.4,
            mean_exec_time_ms: 2.03,
            rows: 4_321,
        },
    ]));
    let mut app = make_app(conn);
    app.current_tab = Tab::TopQueries;

    insta::assert_snapshot!(render_snapshot(&mut app));
}

#[test]
fn top_queries_tab_extension_missing() {
    let mut conn = make_conn();
    conn.set_top_queries(TopQueriesSnapshot::ExtensionMissing);
    let mut app = make_app(conn);
    app.current_tab = Tab::TopQueries;

    insta::assert_snapshot!(render_snapshot(&mut app));
}

#[test]
fn replication_tab_with_two_replicas() {
    let mut conn = make_conn();
    conn.set_replication(vec![
        Replica {
            pid: 9001,
            application_name: Some("walreceiver-1".to_string()),
            client_addr: Some("10.0.0.20".to_string()),
            state: Some("streaming".to_string()),
            sync_state: Some("sync".to_string()),
            replay_lag_secs: Some(0.123),
            sent_lsn: Some("0/3000028".to_string()),
            replay_lsn: Some("0/3000020".to_string()),
        },
        Replica {
            pid: 9002,
            application_name: Some("walreceiver-2".to_string()),
            client_addr: Some("10.0.0.21".to_string()),
            state: Some("streaming".to_string()),
            sync_state: Some("async".to_string()),
            replay_lag_secs: Some(2.5),
            sent_lsn: Some("0/3000028".to_string()),
            replay_lsn: Some("0/2FFFFF8".to_string()),
        },
    ]);
    let mut app = make_app(conn);
    app.current_tab = Tab::Replication;

    insta::assert_snapshot!(render_snapshot(&mut app));
}

#[test]
fn waits_tab_with_aggregated_rows() {
    let mut conn = make_conn();
    conn.waits = vec![
        WaitRow {
            wait_event_type: "Lock".to_string(),
            wait_event: "relation".to_string(),
            count: 17,
        },
        WaitRow {
            wait_event_type: "Client".to_string(),
            wait_event: "ClientRead".to_string(),
            count: 8,
        },
        WaitRow {
            wait_event_type: "IO".to_string(),
            wait_event: "DataFileRead".to_string(),
            count: 3,
        },
    ];
    let mut app = make_app(conn);
    app.current_tab = Tab::Waits;

    insta::assert_snapshot!(render_snapshot(&mut app));
}

#[test]
fn replay_indicator_in_title_bar() {
    let conn = make_conn();
    let mut app = make_app(conn);
    app.is_replay = true;

    insta::assert_snapshot!(render_snapshot(&mut app));
}
