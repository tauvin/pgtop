//! Состояние приложения, которое переживает между кадрами.
//!
//! Phase 8 block A: per-connection state переехал в `ConnectionState`.
//! `App` хранит `Vec<ConnectionState>` + активный индекс. Глобальные
//! UI-вещи (mode, tab, theme, last_action_result) остаются на App.
//! Single-conn режим = `connections.len() == 1`, поведение прежнее.

use std::cmp::Ordering;
use std::collections::VecDeque;

use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::TableState;
use regex::{Regex, RegexBuilder};
use tui_input::{Input, InputRequest};

use crate::actions::ActionResult;
use crate::db::{Backend, Lock, Replica, Stats, TopQueriesSnapshot};
use crate::theme::Theme;

/// Активный таб TUI. Каждый таб — отдельный «view» с собственными данными
/// и хоткеями.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Activity,
    Locks,
    TopQueries,
    Replication,
}

impl Tab {
    pub const fn all() -> &'static [Tab] {
        &[Tab::Activity, Tab::Locks, Tab::TopQueries, Tab::Replication]
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Activity => "Activity",
            Self::Locks => "Locks",
            Self::TopQueries => "Top Queries",
            Self::Replication => "Replication",
        }
    }

    pub fn index(self) -> usize {
        match self {
            Self::Activity => 0,
            Self::Locks => 1,
            Self::TopQueries => 2,
            Self::Replication => 3,
        }
    }

    pub fn from_index(i: usize) -> Option<Tab> {
        Self::all().get(i).copied()
    }
}

/// Модальные состояния UI. Глобальные (один Mode на всё приложение, не
/// per-connection) — модалка открыта поверх всего, какое бы соединение ни
/// было активно. При переключении соединения Mode сбрасывается в Normal
/// (см. `App::set_active`).
#[derive(Debug, Clone)]
pub enum Mode {
    Normal,
    Detail(i32),
    Filter,
    ConfirmCancel(i32),
    ConfirmTerminate(i32, String),
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

/// Состояние одного подключения — собственные данные всех табов + identity
/// (имя, DSN, profile_name, read_only). Phase 8: App хранит вектор таких
/// состояний, переключение между ними — Alt+N (Block B).
#[allow(dead_code)] // `name` и `dsn` подключатся в Block B (UI-индикатор + reconnect-логика).
pub struct ConnectionState {
    /// Имя для display в title/footer. Обычно совпадает с profile_name;
    /// для ad-hoc DSN — может быть просто "default" или host-derived.
    pub name: String,
    pub dsn: String,
    pub read_only: bool,
    pub actions_allowed: bool,
    pub profile_name: Option<String>,

    // Activity tab
    pub backends: Vec<Backend>,
    pub filtered: Vec<usize>,
    pub table_state: TableState,
    pub filter: Filter,
    pub sort: Sort,

    // Locks tab
    pub locks: Vec<Lock>,
    pub locks_table_state: TableState,

    // Top Queries tab
    pub top_queries: TopQueriesSnapshot,
    pub top_queries_table_state: TableState,

    // Replication tab
    pub replication: Vec<Replica>,
    pub replication_table_state: TableState,

    // Header sparklines
    pub stats: StatsHistory,
}

impl ConnectionState {
    pub fn new(
        name: String,
        dsn: String,
        read_only: bool,
        actions_allowed: bool,
        profile_name: Option<String>,
    ) -> Self {
        Self {
            name,
            dsn,
            read_only,
            actions_allowed,
            profile_name,
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
            stats: StatsHistory::default(),
        }
    }

    /// Пересобрать `filtered`-индексы (фильтр + сортировка) и поправить
    /// selection под новый размер.
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

    /// Сдвиг выделения вверх. Принимает `tab` потому что разные табы используют
    /// разные `TableState` поля.
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

/// Корневое состояние приложения. Phase 8: данные per-connection живут в
/// `connections[active]`. Глобальные UI-вещи (mode/tab/theme/result) на App.
pub struct App {
    pub connections: Vec<ConnectionState>,
    /// Индекс активного соединения. Гарантирован валидным —
    /// `set_active` clamps; конструктор требует non-empty Vec.
    pub active: usize,

    pub mode: Mode,
    pub current_tab: Tab,
    pub theme: Theme,
    pub last_action_result: Option<ActionResult>,
}

impl App {
    /// Требует non-empty `connections`. Panic если пустой — это invariant
    /// уровня архитектуры, проверяется в main.rs до вызова.
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

    /// Получить connection по индексу — для adressing'а сообщений из
    /// collector'ов конкретному соединению (Phase 8 Block B).
    #[allow(dead_code)] // wired up in Block B
    pub fn connection_mut(&mut self, idx: usize) -> Option<&mut ConnectionState> {
        self.connections.get_mut(idx)
    }

    /// Set active connection by index. Out-of-bounds → no-op.
    /// Сбрасывает Mode в Normal — модалки привязаны к конкретному соединению,
    /// при переключении они теряют смысл.
    #[allow(dead_code)] // wired up in Block B (Alt+N hotkeys)
    pub fn set_active(&mut self, idx: usize) {
        if idx < self.connections.len() && idx != self.active {
            self.active = idx;
            self.mode = Mode::Normal;
        }
    }

    /// Закрыть модалку, если её pid исчез из активного соединения.
    /// Phase 8 Block B: вызывается из main после `connection_mut(idx).set_backends`,
    /// но только если `idx == app.active` — иначе модалка не относится к
    /// обновляемому коннекту.
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

/// Helper: `clamp_table_state` для не-Activity табов (локs/replication/top_queries
/// — нет filtered-проекции, list напрямую). Activity использует свой clamp в
/// `recompute_filtered` (там есть filter+sort).
fn clamp_table_state(state: &mut TableState, len: usize) {
    match state.selected() {
        _ if len == 0 => state.select(None),
        Some(i) if i >= len => state.select(Some(len - 1)),
        None => state.select(Some(0)),
        Some(_) => {}
    }
}

/// Ring-буферы для sparkline'ов в шапке + последний снапшот для отображения
/// текущего значения. Per-connection (часть ConnectionState).
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
