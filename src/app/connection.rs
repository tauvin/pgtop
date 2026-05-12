//! Per-connection state and supporting types (filter, sort, stats history,
//! waits aggregation). One `ConnectionState` per Postgres session opened by
//! the multi-conn TUI.

use std::cmp::Ordering;
use std::collections::{HashMap, VecDeque};

use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::TableState;
use regex::{Regex, RegexBuilder};
use tui_input::{Input, InputRequest};

use super::tab::{Sort, SortBy, SortDirection, Tab};
use crate::actions::ActionResult;
use crate::db::{Backend, DatabaseStat, Lock, Replica, Stats, TableStat, TopQueriesSnapshot};

#[derive(Default)]
pub struct Filter {
    pub input: Input,
    pub regex: Option<Regex>,
}

impl Filter {
    /// Match a backend against the compiled regex by checking query, user,
    /// state, and database name. Any field hit means the backend stays in
    /// the filtered list. Backends with all four fields empty (rare service
    /// backends) are dropped.
    pub fn matches(&self, b: &Backend) -> bool {
        let Some(re) = &self.regex else {
            return true;
        };
        [
            b.query.as_deref(),
            b.usename.as_deref(),
            b.state.as_deref(),
            b.datname.as_deref(),
        ]
        .iter()
        .any(|field| field.is_some_and(|v| re.is_match(v)))
    }

    pub fn rebuild_regex(&mut self) {
        let value = self.input.value();
        self.regex = if value.is_empty() {
            None
        } else {
            RegexBuilder::new(value).case_insensitive(true).build().ok()
        };
    }

    pub fn clear(&mut self) {
        self.input.reset();
        self.regex = None;
    }
}

/// Connection health indicator. Owned by the activity collector — other
/// collectors reconnect silently.
#[derive(Debug, Clone)]
pub enum ConnectionStatus {
    /// Initial connect or reconnect in progress. `attempt` is 1-based.
    Connecting { attempt: u32 },
    /// Connection is alive and queries are flowing.
    Connected,
}

impl Default for ConnectionStatus {
    fn default() -> Self {
        Self::Connecting { attempt: 1 }
    }
}

/// Per-connection state: identity, health, and tab-specific data.
pub struct ConnectionState {
    pub dsn: String,
    pub read_only: bool,
    pub actions_allowed: bool,
    pub profile_name: Option<String>,
    /// Active backends running longer than this are highlighted as slow
    /// and counted in the Activity tab title.
    pub slow_query_threshold: std::time::Duration,

    /// Current health of the connection.
    pub status: ConnectionStatus,

    pub backends: Vec<Backend>,
    pub filtered: Vec<usize>,
    pub table_state: TableState,
    pub filter: Filter,
    pub sort: Sort,

    pub locks: Vec<Lock>,
    pub locks_table_state: TableState,

    pub top_queries: TopQueriesSnapshot,
    pub top_queries_table_state: TableState,

    pub replication: Vec<Replica>,
    pub replication_table_state: TableState,

    pub databases: Vec<DatabaseStat>,
    pub databases_table_state: TableState,

    pub tables: Vec<TableStat>,
    pub tables_table_state: TableState,

    pub waits: Vec<WaitRow>,
    pub waits_table_state: TableState,

    pub stats: StatsHistory,

    /// Last cancel/terminate action result on this connection. Persisted
    /// per-connection so a result that arrives while the user is on
    /// another conn isn't dropped — switching back surfaces it.
    pub last_action_result: Option<ActionResult>,
}

/// One aggregated row in the Waits histogram. `count` is how many backends
/// in the latest activity snapshot were waiting on this `(type, event)`
/// pair.
#[derive(Debug, Clone)]
pub struct WaitRow {
    pub wait_event_type: String,
    pub wait_event: String,
    pub count: u32,
}

impl ConnectionState {
    pub fn new(
        dsn: String,
        read_only: bool,
        actions_allowed: bool,
        profile_name: Option<String>,
        slow_query_threshold: std::time::Duration,
    ) -> Self {
        Self {
            dsn,
            read_only,
            actions_allowed,
            profile_name,
            slow_query_threshold,
            status: ConnectionStatus::default(),
            backends: Vec::new(),
            filtered: Vec::new(),
            table_state: TableState::default(),
            filter: Filter::default(),
            sort: Sort::default(),
            locks: Vec::new(),
            locks_table_state: TableState::default(),
            top_queries: TopQueriesSnapshot::Loading,
            top_queries_table_state: TableState::default(),
            replication: Vec::new(),
            replication_table_state: TableState::default(),
            databases: Vec::new(),
            databases_table_state: TableState::default(),
            tables: Vec::new(),
            tables_table_state: TableState::default(),
            waits: Vec::new(),
            waits_table_state: TableState::default(),
            stats: StatsHistory::default(),
            last_action_result: None,
        }
    }

    fn recompute_filtered(&mut self) {
        self.filtered = self
            .backends
            .iter()
            .enumerate()
            .filter(|(_, b)| self.filter.matches(b))
            .map(|(i, _)| i)
            .collect();

        let now = Utc::now();
        let by = self.sort.by;
        let dir = self.sort.direction;
        let backends = &self.backends;
        self.filtered.sort_by(|&i, &j| {
            let ord = compare_backends(&backends[i], &backends[j], by, now);
            if dir == SortDirection::Desc {
                ord.reverse()
            } else {
                ord
            }
        });

        clamp_table_state(&mut self.table_state, self.filtered.len());
    }

    pub fn set_backends(&mut self, backends: Vec<Backend>) {
        self.backends = backends;
        self.recompute_filtered();
        self.recompute_waits();
    }

    fn recompute_waits(&mut self) {
        let mut counts: HashMap<(&str, &str), u32> = HashMap::new();
        for b in &self.backends {
            if let (Some(t), Some(e)) = (b.wait_event_type.as_deref(), b.wait_event.as_deref()) {
                *counts.entry((t, e)).or_insert(0) += 1;
            }
        }
        let mut rows: Vec<WaitRow> = counts
            .into_iter()
            .map(|((t, e), c)| WaitRow {
                wait_event_type: t.to_string(),
                wait_event: e.to_string(),
                count: c,
            })
            .collect();
        rows.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then(a.wait_event_type.cmp(&b.wait_event_type))
                .then(a.wait_event.cmp(&b.wait_event))
        });
        self.waits = rows;
        clamp_table_state(&mut self.waits_table_state, self.waits.len());
    }

    pub fn set_locks(&mut self, locks: Vec<Lock>) {
        self.locks = locks;
        let len = self.locks.len();
        clamp_table_state(&mut self.locks_table_state, len);
    }

    pub fn set_top_queries(&mut self, snapshot: TopQueriesSnapshot) {
        self.top_queries = snapshot;
        let len = match &self.top_queries {
            TopQueriesSnapshot::Available(queries) => queries.len(),
            _ => 0,
        };
        clamp_table_state(&mut self.top_queries_table_state, len);
    }

    pub fn set_replication(&mut self, replication: Vec<Replica>) {
        self.replication = replication;
        let len = self.replication.len();
        clamp_table_state(&mut self.replication_table_state, len);
    }

    pub fn set_databases(&mut self, databases: Vec<DatabaseStat>) {
        self.databases = databases;
        let len = self.databases.len();
        clamp_table_state(&mut self.databases_table_state, len);
    }

    pub fn set_tables(&mut self, tables: Vec<TableStat>) {
        self.tables = tables;
        let len = self.tables.len();
        clamp_table_state(&mut self.tables_table_state, len);
    }

    pub fn push_stats(&mut self, stats: Stats) {
        self.stats.push(stats);
    }

    pub fn visible_backend(&self, idx: usize) -> Option<&Backend> {
        self.filtered
            .get(idx)
            .copied()
            .and_then(|i| self.backends.get(i))
    }

    pub fn select_previous(&mut self, tab: Tab) {
        if let Some((state, len)) = self.tab_table(tab)
            && len > 0
        {
            let i = state.selected().map_or(0, |i| i.saturating_sub(1));
            state.select(Some(i));
        }
    }

    pub fn select_next(&mut self, tab: Tab) {
        if let Some((state, len)) = self.tab_table(tab)
            && len > 0
        {
            let max = len - 1;
            let i = state.selected().map_or(0, |i| (i + 1).min(max));
            state.select(Some(i));
        }
    }

    /// Per-tab routing to the `(TableState, row_count)` pair. `None` for
    /// tabs whose data isn't `Available` yet (TopQueries on a server
    /// without `pg_stat_statements`).
    fn tab_table(&mut self, tab: Tab) -> Option<(&mut TableState, usize)> {
        match tab {
            Tab::Activity => Some((&mut self.table_state, self.filtered.len())),
            Tab::Locks => Some((&mut self.locks_table_state, self.locks.len())),
            Tab::TopQueries => match &self.top_queries {
                TopQueriesSnapshot::Available(queries) => {
                    let len = queries.len();
                    Some((&mut self.top_queries_table_state, len))
                }
                _ => None,
            },
            Tab::Replication => Some((&mut self.replication_table_state, self.replication.len())),
            Tab::Databases => Some((&mut self.databases_table_state, self.databases.len())),
            Tab::Tables => Some((&mut self.tables_table_state, self.tables.len())),
            Tab::Waits => Some((&mut self.waits_table_state, self.waits.len())),
        }
    }

    pub fn cycle_sort_column(&mut self) {
        self.sort.by = self.sort.by.next();
        self.recompute_filtered();
    }

    pub fn toggle_sort_direction(&mut self) {
        self.sort.direction = self.sort.direction.flip();
        self.recompute_filtered();
    }

    pub fn handle_filter_input(&mut self, key: KeyEvent) {
        let Some(req) = key_to_request(key) else {
            return;
        };
        if self.filter.input.handle(req).is_some() {
            self.filter.rebuild_regex();
            self.recompute_filtered();
        }
    }

    pub fn clear_filter(&mut self) {
        self.filter.clear();
        self.recompute_filtered();
    }
}

pub(super) fn clamp_table_state(state: &mut TableState, len: usize) {
    match state.selected() {
        _ if len == 0 => state.select(None),
        Some(i) if i >= len => state.select(Some(len - 1)),
        None => state.select(Some(0)),
        Some(_) => {}
    }
}

/// Ring buffers for header sparklines plus the latest snapshot for current
/// values. Per-connection.
pub struct StatsHistory {
    pub tps: VecDeque<f64>,
    pub conns: VecDeque<u32>,
    pub cache_hit: VecDeque<f64>,
    pub current: Option<Stats>,
}

const STATS_HISTORY_LEN: usize = 60;

impl Default for StatsHistory {
    fn default() -> Self {
        Self {
            tps: VecDeque::with_capacity(STATS_HISTORY_LEN),
            conns: VecDeque::with_capacity(STATS_HISTORY_LEN),
            cache_hit: VecDeque::with_capacity(STATS_HISTORY_LEN),
            current: None,
        }
    }
}

impl StatsHistory {
    pub fn push(&mut self, stats: Stats) {
        push_bounded(&mut self.tps, stats.tps);
        push_bounded(&mut self.conns, stats.active_connections);
        push_bounded(&mut self.cache_hit, stats.cache_hit_pct);
        self.current = Some(stats);
    }
}

fn push_bounded<T>(buf: &mut VecDeque<T>, value: T) {
    buf.push_back(value);
    if buf.len() > STATS_HISTORY_LEN {
        buf.pop_front();
    }
}

pub(super) fn compare_backends(
    a: &Backend,
    b: &Backend,
    by: SortBy,
    now: DateTime<Utc>,
) -> Ordering {
    match by {
        SortBy::Pid => a.pid.cmp(&b.pid),
        SortBy::User => a.usename.cmp(&b.usename),
        SortBy::State => a.state.cmp(&b.state),
        SortBy::Wait => (a.wait_event_type.as_deref(), a.wait_event.as_deref())
            .cmp(&(b.wait_event_type.as_deref(), b.wait_event.as_deref())),
        SortBy::Duration => {
            let da = a.query_start.map(|s| now - s);
            let db = b.query_start.map(|s| now - s);
            da.cmp(&db)
        }
        SortBy::Query => a.query.cmp(&b.query),
    }
}

fn key_to_request(key: KeyEvent) -> Option<InputRequest> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    match key.code {
        KeyCode::Char(c) if !ctrl => Some(InputRequest::InsertChar(c)),
        KeyCode::Char(c) if ctrl => match c {
            'a' | 'A' => Some(InputRequest::GoToStart),
            'e' | 'E' => Some(InputRequest::GoToEnd),
            'u' | 'U' => Some(InputRequest::DeleteLine),
            'w' | 'W' => Some(InputRequest::DeletePrevWord),
            _ => None,
        },
        KeyCode::Backspace => Some(InputRequest::DeletePrevChar),
        KeyCode::Delete => Some(InputRequest::DeleteNextChar),
        KeyCode::Left => Some(InputRequest::GoToPrevChar),
        KeyCode::Right => Some(InputRequest::GoToNextChar),
        KeyCode::Home => Some(InputRequest::GoToStart),
        KeyCode::End => Some(InputRequest::GoToEnd),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn epoch() -> DateTime<Utc> {
        Utc.timestamp_opt(0, 0).unwrap()
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

    // Filter

    #[test]
    fn filter_default_matches_everything() {
        let f = Filter::default();
        assert!(f.matches(&backend(1)));
        let mut b = backend(2);
        b.query = Some("SELECT 1".to_string());
        assert!(f.matches(&b));
    }

    #[test]
    fn filter_matches_substring_case_insensitive() {
        let mut f = Filter::default();
        f.input = "select".into();
        f.rebuild_regex();

        let mut b = backend(1);
        b.query = Some("SELECT * FROM t".to_string());
        assert!(f.matches(&b));

        b.query = Some("delete from t".to_string());
        assert!(!f.matches(&b));
    }

    #[test]
    fn filter_drops_rows_without_query() {
        let mut f = Filter::default();
        f.input = "x".into();
        f.rebuild_regex();
        assert!(!f.matches(&backend(1)));
    }

    #[test]
    fn filter_matches_username() {
        let mut f = Filter::default();
        f.input = "alice".into();
        f.rebuild_regex();

        let mut b = backend(1);
        b.usename = Some("alice".to_string());
        assert!(f.matches(&b));

        b.usename = Some("bob".to_string());
        assert!(!f.matches(&b));
    }

    #[test]
    fn filter_matches_state_or_datname() {
        let mut f = Filter::default();
        f.input = "idle".into();
        f.rebuild_regex();
        let mut b = backend(1);
        b.state = Some("idle in transaction".to_string());
        assert!(f.matches(&b));

        let mut f = Filter::default();
        f.input = "prod".into();
        f.rebuild_regex();
        let mut b = backend(2);
        b.datname = Some("catalog_prod".to_string());
        assert!(f.matches(&b));
    }

    #[test]
    fn filter_rebuild_handles_invalid_regex_gracefully() {
        let mut f = Filter::default();
        f.input = "[".into();
        f.rebuild_regex();
        assert!(f.regex.is_none());
        assert!(f.matches(&backend(1)));
    }

    #[test]
    fn filter_clear_resets_regex_and_input() {
        let mut f = Filter::default();
        f.input = "x".into();
        f.rebuild_regex();
        assert!(f.regex.is_some());

        f.clear();
        assert!(f.regex.is_none());
        assert_eq!(f.input.value(), "");
    }

    // compare_backends

    #[test]
    fn compare_backends_pid_ascending() {
        let a = backend(10);
        let b = backend(20);
        assert_eq!(
            compare_backends(&a, &b, SortBy::Pid, epoch()),
            Ordering::Less
        );
    }

    #[test]
    fn compare_backends_user_alphabetical_with_nulls_last() {
        let mut a = backend(1);
        a.usename = Some("alice".to_string());
        let mut b = backend(2);
        b.usename = Some("bob".to_string());
        assert_eq!(
            compare_backends(&a, &b, SortBy::User, epoch()),
            Ordering::Less
        );

        // Option<T>: None < Some(_), so a NULL usename sorts before any name.
        let c = backend(3);
        assert_eq!(
            compare_backends(&c, &a, SortBy::User, epoch()),
            Ordering::Less
        );
    }

    #[test]
    fn compare_backends_duration_uses_query_start() {
        let now = Utc.timestamp_opt(1_000_000, 0).unwrap();
        let mut older = backend(1);
        older.query_start = Some(now - chrono::Duration::seconds(60));
        let mut younger = backend(2);
        younger.query_start = Some(now - chrono::Duration::seconds(5));

        // Older query → larger `now - start` → greater duration.
        assert_eq!(
            compare_backends(&older, &younger, SortBy::Duration, now),
            Ordering::Greater
        );
    }

    #[test]
    fn compare_backends_wait_compares_pair() {
        let mut a = backend(1);
        a.wait_event_type = Some("Lock".to_string());
        a.wait_event = Some("relation".to_string());

        let mut b = backend(2);
        b.wait_event_type = Some("Lock".to_string());
        b.wait_event = Some("transactionid".to_string());

        assert_eq!(
            compare_backends(&a, &b, SortBy::Wait, epoch()),
            Ordering::Less
        );
    }

    // Waits aggregate

    fn waiting(pid: i32, t: &str, e: &str) -> Backend {
        let mut b = backend(pid);
        b.wait_event_type = Some(t.to_string());
        b.wait_event = Some(e.to_string());
        b
    }

    fn conn() -> ConnectionState {
        ConnectionState::new(
            "".into(),
            false,
            false,
            None,
            std::time::Duration::from_secs(30),
        )
    }

    #[test]
    fn recompute_waits_groups_and_counts() {
        let mut c = conn();
        c.set_backends(vec![
            waiting(1, "Lock", "relation"),
            waiting(2, "Lock", "relation"),
            waiting(3, "Client", "ClientRead"),
            backend(4),
        ]);

        assert_eq!(c.waits.len(), 2);
        assert_eq!(c.waits[0].wait_event_type, "Lock");
        assert_eq!(c.waits[0].wait_event, "relation");
        assert_eq!(c.waits[0].count, 2);
        assert_eq!(c.waits[1].wait_event_type, "Client");
        assert_eq!(c.waits[1].count, 1);
    }

    #[test]
    fn recompute_waits_skips_idle_backends() {
        let mut c = conn();
        c.set_backends(vec![backend(1), backend(2)]);
        assert!(c.waits.is_empty());
    }

    #[test]
    fn recompute_waits_sorts_by_count_then_alpha() {
        let mut c = conn();
        c.set_backends(vec![
            waiting(1, "Z", "a"),
            waiting(2, "A", "x"),
            waiting(3, "A", "x"),
        ]);

        assert_eq!(c.waits[0].count, 2);
        assert_eq!(c.waits[0].wait_event_type, "A");
        assert_eq!(c.waits[1].wait_event_type, "Z");
    }
}
