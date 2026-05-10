//! Application state shared across frames. Owns per-connection state plus
//! global UI state (mode, current tab, theme, last action result).

use std::cmp::Ordering;
use std::collections::{HashMap, VecDeque};

use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::TableState;
use regex::{Regex, RegexBuilder};
use tui_input::{Input, InputRequest};

use crate::actions::ActionResult;
use crate::db::{Backend, DatabaseStat, Lock, Replica, Stats, TableStat, TopQueriesSnapshot};
use crate::theme::Theme;

/// Active TUI tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Activity,
    Locks,
    TopQueries,
    Replication,
    Databases,
    Tables,
    Waits,
}

impl Tab {
    pub const fn all() -> &'static [Tab] {
        &[
            Tab::Activity,
            Tab::Locks,
            Tab::TopQueries,
            Tab::Replication,
            Tab::Databases,
            Tab::Tables,
            Tab::Waits,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Activity => "Activity",
            Self::Locks => "Locks",
            Self::TopQueries => "Top Queries",
            Self::Replication => "Replication",
            Self::Databases => "Databases",
            Self::Tables => "Tables",
            Self::Waits => "Waits",
        }
    }

    pub fn index(self) -> usize {
        match self {
            Self::Activity => 0,
            Self::Locks => 1,
            Self::TopQueries => 2,
            Self::Replication => 3,
            Self::Databases => 4,
            Self::Tables => 5,
            Self::Waits => 6,
        }
    }

    pub fn from_index(i: usize) -> Option<Tab> {
        Self::all().get(i).copied()
    }
}

/// Modal UI state. Global across the app — switching connections resets
/// `Mode` to `Normal` (see `App::set_active`).
#[derive(Debug, Clone)]
pub enum Mode {
    Normal,
    Detail(i32),
    Filter,
    ConfirmCancel(i32),
    ConfirmTerminate(i32, String),
    Explain(ExplainPopup),
}

/// State of the EXPLAIN popup: `Loading` while the query runs, `Ready`
/// with the plan text, or `Error` with the SQL error message.
#[derive(Debug, Clone)]
pub enum ExplainPopup {
    Loading { pid: i32 },
    Ready { pid: i32, plan: String },
    Error { pid: i32, message: String },
}

#[derive(Default)]
pub struct Filter {
    pub input: Input,
    pub regex: Option<Regex>,
}

impl Filter {
    pub fn matches(&self, b: &Backend) -> bool {
        let Some(re) = &self.regex else {
            return true;
        };
        b.query.as_deref().is_some_and(|q| re.is_match(q))
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortBy {
    Pid,
    User,
    State,
    Wait,
    Duration,
    Query,
}

impl SortBy {
    pub fn next(self) -> Self {
        match self {
            Self::Pid => Self::User,
            Self::User => Self::State,
            Self::State => Self::Wait,
            Self::Wait => Self::Duration,
            Self::Duration => Self::Query,
            Self::Query => Self::Pid,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Pid => "pid",
            Self::User => "user",
            Self::State => "state",
            Self::Wait => "wait",
            Self::Duration => "duration",
            Self::Query => "query",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    Asc,
    Desc,
}

impl SortDirection {
    pub fn flip(self) -> Self {
        match self {
            Self::Asc => Self::Desc,
            Self::Desc => Self::Asc,
        }
    }

    pub fn arrow(self) -> &'static str {
        match self {
            Self::Asc => "▲",
            Self::Desc => "▼",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Sort {
    pub by: SortBy,
    pub direction: SortDirection,
}

impl Default for Sort {
    fn default() -> Self {
        Self {
            by: SortBy::Pid,
            direction: SortDirection::Asc,
        }
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
#[allow(dead_code)]
pub struct ConnectionState {
    /// Display name (usually the profile name; "default" otherwise).
    pub name: String,
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
        name: String,
        dsn: String,
        read_only: bool,
        actions_allowed: bool,
        profile_name: Option<String>,
        slow_query_threshold: std::time::Duration,
    ) -> Self {
        Self {
            name,
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

        let len = self.filtered.len();
        match self.table_state.selected() {
            _ if len == 0 => self.table_state.select(None),
            Some(i) if i >= len => self.table_state.select(Some(len - 1)),
            None => self.table_state.select(Some(0)),
            Some(_) => {}
        }
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

    pub fn visible_backends(&self) -> impl Iterator<Item = &Backend> + '_ {
        self.filtered.iter().filter_map(|&i| self.backends.get(i))
    }

    pub fn select_previous(&mut self, tab: Tab) {
        match tab {
            Tab::Activity => {
                if self.filtered.is_empty() {
                    return;
                }
                let i = self
                    .table_state
                    .selected()
                    .map_or(0, |i| i.saturating_sub(1));
                self.table_state.select(Some(i));
            }
            Tab::Locks => {
                if self.locks.is_empty() {
                    return;
                }
                let i = self
                    .locks_table_state
                    .selected()
                    .map_or(0, |i| i.saturating_sub(1));
                self.locks_table_state.select(Some(i));
            }
            Tab::TopQueries => {
                let TopQueriesSnapshot::Available(queries) = &self.top_queries else {
                    return;
                };
                if queries.is_empty() {
                    return;
                }
                let i = self
                    .top_queries_table_state
                    .selected()
                    .map_or(0, |i| i.saturating_sub(1));
                self.top_queries_table_state.select(Some(i));
            }
            Tab::Replication => {
                if self.replication.is_empty() {
                    return;
                }
                let i = self
                    .replication_table_state
                    .selected()
                    .map_or(0, |i| i.saturating_sub(1));
                self.replication_table_state.select(Some(i));
            }
            Tab::Databases => {
                if self.databases.is_empty() {
                    return;
                }
                let i = self
                    .databases_table_state
                    .selected()
                    .map_or(0, |i| i.saturating_sub(1));
                self.databases_table_state.select(Some(i));
            }
            Tab::Tables => {
                if self.tables.is_empty() {
                    return;
                }
                let i = self
                    .tables_table_state
                    .selected()
                    .map_or(0, |i| i.saturating_sub(1));
                self.tables_table_state.select(Some(i));
            }
            Tab::Waits => {
                if self.waits.is_empty() {
                    return;
                }
                let i = self
                    .waits_table_state
                    .selected()
                    .map_or(0, |i| i.saturating_sub(1));
                self.waits_table_state.select(Some(i));
            }
        }
    }

    pub fn select_next(&mut self, tab: Tab) {
        match tab {
            Tab::Activity => {
                if self.filtered.is_empty() {
                    return;
                }
                let max = self.filtered.len() - 1;
                let i = self.table_state.selected().map_or(0, |i| (i + 1).min(max));
                self.table_state.select(Some(i));
            }
            Tab::Locks => {
                if self.locks.is_empty() {
                    return;
                }
                let max = self.locks.len() - 1;
                let i = self
                    .locks_table_state
                    .selected()
                    .map_or(0, |i| (i + 1).min(max));
                self.locks_table_state.select(Some(i));
            }
            Tab::TopQueries => {
                let TopQueriesSnapshot::Available(queries) = &self.top_queries else {
                    return;
                };
                if queries.is_empty() {
                    return;
                }
                let max = queries.len() - 1;
                let i = self
                    .top_queries_table_state
                    .selected()
                    .map_or(0, |i| (i + 1).min(max));
                self.top_queries_table_state.select(Some(i));
            }
            Tab::Replication => {
                if self.replication.is_empty() {
                    return;
                }
                let max = self.replication.len() - 1;
                let i = self
                    .replication_table_state
                    .selected()
                    .map_or(0, |i| (i + 1).min(max));
                self.replication_table_state.select(Some(i));
            }
            Tab::Databases => {
                if self.databases.is_empty() {
                    return;
                }
                let max = self.databases.len() - 1;
                let i = self
                    .databases_table_state
                    .selected()
                    .map_or(0, |i| (i + 1).min(max));
                self.databases_table_state.select(Some(i));
            }
            Tab::Tables => {
                if self.tables.is_empty() {
                    return;
                }
                let max = self.tables.len() - 1;
                let i = self
                    .tables_table_state
                    .selected()
                    .map_or(0, |i| (i + 1).min(max));
                self.tables_table_state.select(Some(i));
            }
            Tab::Waits => {
                if self.waits.is_empty() {
                    return;
                }
                let max = self.waits.len() - 1;
                let i = self
                    .waits_table_state
                    .selected()
                    .map_or(0, |i| (i + 1).min(max));
                self.waits_table_state.select(Some(i));
            }
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

/// Root application state.
pub struct App {
    pub connections: Vec<ConnectionState>,
    /// Index of the active connection. Always valid — `set_active` clamps and
    /// the constructor requires a non-empty `Vec`.
    pub active: usize,

    pub mode: Mode,
    pub current_tab: Tab,
    pub theme: Theme,
    pub last_action_result: Option<ActionResult>,
}

impl App {
    /// Requires a non-empty `connections`. Panics otherwise.
    pub fn new(connections: Vec<ConnectionState>) -> Self {
        assert!(
            !connections.is_empty(),
            "App requires at least one connection"
        );
        Self {
            connections,
            active: 0,
            mode: Mode::Normal,
            current_tab: Tab::Activity,
            theme: Theme::default(),
            last_action_result: None,
        }
    }

    pub fn active(&self) -> &ConnectionState {
        &self.connections[self.active]
    }

    pub fn active_mut(&mut self) -> &mut ConnectionState {
        &mut self.connections[self.active]
    }

    #[allow(dead_code)]
    pub fn connection_mut(&mut self, idx: usize) -> Option<&mut ConnectionState> {
        self.connections.get_mut(idx)
    }

    /// Set the active connection by index. Out-of-bounds is a no-op.
    /// Resets `Mode` to `Normal`.
    #[allow(dead_code)]
    pub fn set_active(&mut self, idx: usize) {
        if idx < self.connections.len() && idx != self.active {
            self.active = idx;
            self.mode = Mode::Normal;
        }
    }

    /// Close the active modal if its pid is no longer present in the active
    /// connection's backends.
    pub fn maybe_close_dead_modal(&mut self) {
        let active_pid = match &self.mode {
            Mode::Detail(pid) | Mode::ConfirmCancel(pid) => Some(*pid),
            Mode::ConfirmTerminate(pid, _) => Some(*pid),
            _ => None,
        };
        if let Some(pid) = active_pid
            && !self.active().backends.iter().any(|b| b.pid == pid)
        {
            self.mode = Mode::Normal;
        }
    }

    pub fn select_previous(&mut self) {
        let tab = self.current_tab;
        self.active_mut().select_previous(tab);
    }

    pub fn select_next(&mut self) {
        let tab = self.current_tab;
        self.active_mut().select_next(tab);
    }

    pub fn cycle_sort_column(&mut self) {
        self.active_mut().cycle_sort_column();
    }

    pub fn toggle_sort_direction(&mut self) {
        self.active_mut().toggle_sort_direction();
    }

    pub fn handle_filter_input(&mut self, key: KeyEvent) {
        self.active_mut().handle_filter_input(key);
    }

    pub fn enter_filter_mode(&mut self) {
        self.mode = Mode::Filter;
    }

    pub fn exit_filter_mode(&mut self, commit: bool) {
        if !commit {
            self.active_mut().clear_filter();
        }
        self.mode = Mode::Normal;
    }

    pub fn on_enter(&mut self) {
        let conn = self.active();
        if let Some(idx) = conn.table_state.selected()
            && let Some(b) = conn.visible_backend(idx)
        {
            self.mode = Mode::Detail(b.pid);
        }
    }

    /// Selected backend's `(pid, query)` if Activity has a row selected and
    /// the backend has a non-empty query. Used to drive the EXPLAIN popup.
    pub fn selected_query(&self) -> Option<(i32, String)> {
        let conn = self.active();
        let idx = conn.table_state.selected()?;
        let b = conn.visible_backend(idx)?;
        let q = b.query.as_ref()?;
        if q.trim().is_empty() {
            return None;
        }
        Some((b.pid, q.clone()))
    }

    pub fn close_modal(&mut self) {
        self.mode = Mode::Normal;
    }

    pub fn try_open_confirm_cancel(&mut self) -> bool {
        if self.current_tab != Tab::Activity {
            return false;
        }
        let conn = self.active();
        if !conn.actions_allowed {
            return false;
        }
        let Some(idx) = conn.table_state.selected() else {
            return false;
        };
        let Some(b) = conn.visible_backend(idx) else {
            return false;
        };
        if b.is_self() {
            return false;
        }
        self.mode = Mode::ConfirmCancel(b.pid);
        true
    }

    pub fn try_open_confirm_terminate(&mut self) -> bool {
        if self.current_tab != Tab::Activity {
            return false;
        }
        let conn = self.active();
        if !conn.actions_allowed {
            return false;
        }
        let Some(idx) = conn.table_state.selected() else {
            return false;
        };
        let Some(b) = conn.visible_backend(idx) else {
            return false;
        };
        if b.is_self() {
            return false;
        }
        self.mode = Mode::ConfirmTerminate(b.pid, String::new());
        true
    }

    pub fn terminate_input_push(&mut self, c: char) {
        if let Mode::ConfirmTerminate(_, text) = &mut self.mode {
            text.push(c);
        }
    }

    pub fn terminate_input_backspace(&mut self) {
        if let Mode::ConfirmTerminate(_, text) = &mut self.mode {
            text.pop();
        }
    }

    pub fn try_confirm_terminate(&mut self) -> Option<i32> {
        if let Mode::ConfirmTerminate(pid, text) = &self.mode
            && text == "yes"
        {
            let pid = *pid;
            self.close_modal();
            return Some(pid);
        }
        None
    }

    pub fn set_action_result(&mut self, result: ActionResult) {
        self.last_action_result = Some(result);
    }

    pub fn set_tab(&mut self, tab: Tab) {
        self.current_tab = tab;
    }

    pub fn next_tab(&mut self) {
        let next = (self.current_tab.index() + 1) % Tab::all().len();
        self.current_tab = Tab::from_index(next).unwrap();
    }
}

fn clamp_table_state(state: &mut TableState, len: usize) {
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

fn compare_backends(a: &Backend, b: &Backend, by: SortBy, now: chrono::DateTime<Utc>) -> Ordering {
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
    use chrono::{DateTime, TimeZone};

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

    // SortBy / SortDirection

    #[test]
    fn sort_by_cycles_through_all_columns() {
        let mut s = SortBy::Pid;
        let mut seen = vec![s];
        for _ in 0..6 {
            s = s.next();
            seen.push(s);
        }
        assert_eq!(
            seen,
            vec![
                SortBy::Pid,
                SortBy::User,
                SortBy::State,
                SortBy::Wait,
                SortBy::Duration,
                SortBy::Query,
                SortBy::Pid,
            ]
        );
    }

    #[test]
    fn sort_direction_flip_round_trips() {
        assert_eq!(SortDirection::Asc.flip(), SortDirection::Desc);
        assert_eq!(SortDirection::Asc.flip().flip(), SortDirection::Asc);
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
            "t".into(),
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
