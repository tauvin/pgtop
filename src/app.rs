//! Состояние приложения, которое переживает между кадрами.
//!
//! Каждый кадр UI получает `&mut App`. Обновления данных от collector'а заходят
//! в `App::set_backends`. На Phase 4 block C добавилось поле `filter`
//! (regex-фильтр по тексту query) и `filtered` (предвычисленные индексы
//! видимых backend'ов).

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
/// и хоткеями. `index()` соответствует позиции в `Tab::all()` (для tab bar).
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

/// Модальные состояния UI.
///
/// `Detail(pid)` хранит **pid**, а не индекс: список перетасуется, индекс
/// «уплывёт», а pid стабилен. Если pid исчезнет, `set_backends` авто-возвращает
/// в `Normal`.
///
/// `Filter` — режим редактирования фильтра. Сам текст и regex живут в
/// `App.filter`, чтобы переживать выход из режима (Enter коммитит, Esc отменяет).
///
/// `ConfirmCancel(pid)` — модалка подтверждения cancel-action на выбранном
/// backend'е. Enter → отправить команду, Esc → отменить.
///
/// `ConfirmTerminate(pid, String)` — destructive terminate. String хранит
/// набранный пользователем текст; команда отправляется только если `== "yes"`.
/// Это анти-fool design: невозможно случайно подтвердить, нужно явно
/// набрать три буквы.
#[derive(Debug, Clone)]
pub enum Mode {
    Normal,
    Detail(i32),
    Filter,
    ConfirmCancel(i32),
    ConfirmTerminate(i32, String),
}

/// Состояние regex-фильтра. Применяется к полю `query` каждого backend'а.
///
/// `input` хранит сырой текст (управляется `tui-input`'ом — поддерживает
/// курсор, backspace, ctrl+u и т.д.). `regex` — последняя успешно
/// скомпилированная версия. Если входной текст невалидный, `regex` = None
/// и фильтр временно не применяется (как «поиск отключён» — показываем всё).
#[derive(Default)]
pub struct Filter {
    pub input: Input,
    pub regex: Option<Regex>,
}

impl Filter {
    /// `true`, если backend проходит фильтр. Без regex — все проходят.
    /// С regex — проверяем по тексту query (NULL query = не проходит).
    pub fn matches(&self, b: &Backend) -> bool {
        let Some(re) = &self.regex else {
            return true;
        };
        b.query.as_deref().is_some_and(|q| re.is_match(q))
    }

    /// Перекомпилировать regex из текущего `input.value()`.
    /// Пустая строка → нет фильтра. Невалидный regex → `None` (тоже без фильтра,
    /// UI показывает индикатор «invalid»).
    pub fn rebuild_regex(&mut self) {
        let value = self.input.value();
        self.regex = if value.is_empty() {
            None
        } else {
            // Case-insensitive — типичная UX для interactive search.
            // Пользователь может явно опт-аутнуть префиксом `(?-i)`.
            RegexBuilder::new(value).case_insensitive(true).build().ok()
        };
    }

    /// Полностью очистить: input + regex. Используется в Esc-cancel сценарии.
    pub fn clear(&mut self) {
        self.input.reset();
        self.regex = None;
    }
}

/// Колонка таблицы для сортировки. Один enum-вариант на колонку — порядок
/// `next()` совпадает с порядком колонок в UI слева направо.
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
    /// Циклический shift на следующую колонку (Query → Pid). Используется
    /// для хоткея `s`.
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

    /// Заголовок колонки в таблице.
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

    /// Юникод-стрелка для индикатора в header'е.
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

/// Корневое состояние приложения.
pub struct App {
    /// Полный snapshot pg_stat_activity от collector'а.
    pub backends: Vec<Backend>,

    /// Индексы backend'ов, прошедших фильтр и отсортированных по `sort`.
    /// `TableState.selected` индексирует этот вектор (а не `backends`).
    /// Пересчитывается в `recompute_filtered` после смены данных, фильтра
    /// или сортировки.
    pub filtered: Vec<usize>,

    pub table_state: TableState,

    // --- Locks tab data (Phase 5 block B) ---
    pub locks: Vec<Lock>,
    pub locks_table_state: TableState,

    // --- Top Queries tab data (Phase 5 block C) ---
    pub top_queries: TopQueriesSnapshot,
    pub top_queries_table_state: TableState,

    // --- Replication tab data (Phase 5 block D) ---
    pub replication: Vec<Replica>,
    pub replication_table_state: TableState,

    // --- Header sparklines (Phase 5 block E) ---
    pub stats: StatsHistory,

    // --- Глобальное состояние ---
    pub mode: Mode,
    pub filter: Filter,
    pub sort: Sort,
    pub current_tab: Tab,

    /// Phase 6: разрешены ли cancel/terminate-actions. Финальное resolved-значение
    /// после layered config loading'а — учитывает CLI `--allow-actions`,
    /// CLI `--read-only`, `profile.read_only`. См. `config::Resolved::from_layers`.
    pub actions_allowed: bool,

    /// Последний результат action'а от executor'а — отображается в status-line
    /// (filter-line area). `None` пока ни одной команды не было; иначе самый
    /// свежий результат.
    pub last_action_result: Option<ActionResult>,

    /// Phase 7: имя активного профиля для отображения в title-bar. `None` —
    /// если запущено без профиля (только CLI/env).
    pub profile_name: Option<String>,

    /// Phase 7: read-only mode для UI-индикации. Сама блокировка actions'ов
    /// уже учтена в `actions_allowed`; это поле — только для отображения.
    pub read_only: bool,

    /// Phase 7: семантические цвета (Theme — Copy, cheap для копий
    /// при передаче в render-функции).
    pub theme: Theme,
}

impl App {
    pub fn new(actions_allowed: bool) -> Self {
        Self {
            backends: Vec::new(),
            filtered: Vec::new(),
            table_state: TableState::default(),
            locks: Vec::new(),
            locks_table_state: TableState::default(),
            top_queries: TopQueriesSnapshot::Loading,
            top_queries_table_state: TableState::default(),
            replication: Vec::new(),
            replication_table_state: TableState::default(),
            stats: StatsHistory::default(),
            mode: Mode::Normal,
            filter: Filter::default(),
            sort: Sort::default(),
            current_tab: Tab::Activity,
            actions_allowed,
            last_action_result: None,
            profile_name: None,
            read_only: false,
            theme: Theme::default(),
        }
    }

    /// Пересобрать `filtered`-индексы (фильтр + сортировка) и поправить
    /// selection под новый размер. Вызывается при любом изменении
    /// `backends`, `filter` или `sort`.
    fn recompute_filtered(&mut self) {
        // Шаг 1: фильтрация.
        self.filtered = self
            .backends
            .iter()
            .enumerate()
            .filter(|(_, b)| self.filter.matches(b))
            .map(|(i, _)| i)
            .collect();

        // Шаг 2: сортировка по выбранной колонке + направлению.
        // `now` фиксируем один раз на проход — иначе компаратор может
        // сравнивать по дрейфующему «сейчас» в одной сортировке.
        // `&self.backends` (immutable) и `&mut self.filtered` — disjoint field
        // borrows; компилятор пропустит благодаря borrow splitting'у.
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

        // Шаг 3: clamp selection под новую длину.
        let len = self.filtered.len();
        match self.table_state.selected() {
            _ if len == 0 => self.table_state.select(None),
            Some(i) if i >= len => self.table_state.select(Some(len - 1)),
            None => self.table_state.select(Some(0)),
            Some(_) => {}
        }
    }

    /// Обновить snapshot. Заодно пересчитываем filtered и закрываем модалку
    /// (Detail/ConfirmCancel), если её pid исчез из снапшота.
    pub fn set_backends(&mut self, backends: Vec<Backend>) {
        self.backends = backends;
        self.recompute_filtered();

        // Все pid-bound модалки авто-закрываются на смерть выбранного pid'а:
        // Detail/ConfirmCancel/ConfirmTerminate — общая логика.
        let active_pid = match &self.mode {
            Mode::Detail(pid) | Mode::ConfirmCancel(pid) => Some(*pid),
            Mode::ConfirmTerminate(pid, _) => Some(*pid),
            _ => None,
        };
        if let Some(pid) = active_pid
            && !self.backends.iter().any(|b| b.pid == pid)
        {
            self.mode = Mode::Normal;
        }
    }

    /// Получить видимый (после фильтра) backend по индексу из `filtered`.
    pub fn visible_backend(&self, idx: usize) -> Option<&Backend> {
        self.filtered
            .get(idx)
            .copied()
            .and_then(|i| self.backends.get(i))
    }

    /// Итератор по видимым backend'ам — для render_table.
    /// `+ '_` — лайфтайм возвращаемого `impl Iterator` привязан к `&self`.
    pub fn visible_backends(&self) -> impl Iterator<Item = &Backend> + '_ {
        self.filtered.iter().filter_map(|&i| self.backends.get(i))
    }

    /// Сдвиг выделения вверх. Диспатчится на TableState текущего таба
    /// (Activity → app.table_state, Locks → app.locks_table_state). На
    /// табах без list-content (TopQueries/Replication пока) — no-op.
    pub fn select_previous(&mut self) {
        match self.current_tab {
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

    /// Сдвиг выделения вниз. Структурно зеркальный `select_previous`.
    pub fn select_next(&mut self) {
        match self.current_tab {
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

    /// Обновить snapshot блокировок. Selection клампится по тем же правилам,
    /// что в `set_backends` (пусто → None; selected ≥ len → последняя; None +
    /// данные → первая).
    pub fn set_locks(&mut self, locks: Vec<Lock>) {
        self.locks = locks;
        let len = self.locks.len();
        match self.locks_table_state.selected() {
            _ if len == 0 => self.locks_table_state.select(None),
            Some(i) if i >= len => self.locks_table_state.select(Some(len - 1)),
            None => self.locks_table_state.select(Some(0)),
            Some(_) => {}
        }
    }

    /// Обновить snapshot Top Queries. На non-Available состояниях
    /// (Loading / ExtensionMissing) сбрасываем selection в None.
    pub fn set_top_queries(&mut self, snapshot: TopQueriesSnapshot) {
        self.top_queries = snapshot;
        let len = match &self.top_queries {
            TopQueriesSnapshot::Available(queries) => queries.len(),
            _ => 0,
        };
        match self.top_queries_table_state.selected() {
            _ if len == 0 => self.top_queries_table_state.select(None),
            Some(i) if i >= len => self.top_queries_table_state.select(Some(len - 1)),
            None => self.top_queries_table_state.select(Some(0)),
            Some(_) => {}
        }
    }

    /// Обновить snapshot Replication. Те же clamp-правила, что в `set_locks`.
    pub fn set_replication(&mut self, replication: Vec<Replica>) {
        self.replication = replication;
        let len = self.replication.len();
        match self.replication_table_state.selected() {
            _ if len == 0 => self.replication_table_state.select(None),
            Some(i) if i >= len => self.replication_table_state.select(Some(len - 1)),
            None => self.replication_table_state.select(Some(0)),
            Some(_) => {}
        }
    }

    /// Запушить новые stats в ring-buffer'ы для sparkline'ов в шапке.
    /// Также обновить `current` для текущего значения в подписи.
    pub fn push_stats(&mut self, stats: Stats) {
        self.stats.push(stats);
    }

    /// Enter на выбранной строке: открыть detail view.
    /// Используем `visible_backend` — selected индексирует filtered, не backends.
    pub fn on_enter(&mut self) {
        if let Some(idx) = self.table_state.selected()
            && let Some(b) = self.visible_backend(idx)
        {
            self.mode = Mode::Detail(b.pid);
        }
    }

    /// Закрыть Detail / другую модалку, вернуться в Normal.
    pub fn close_modal(&mut self) {
        self.mode = Mode::Normal;
    }

    /// Хоткей `c` (Activity, actions_allowed): открыть confirm-cancel модалку
    /// для выбранного backend'а. Игнорируем self-backend'ы (pgtop'овские
    /// собственные соединения) — нельзя cancel'ить свой же запрос.
    /// Возвращает true если модалка открылась — main по этому смотрит,
    /// нужно ли что-то ещё делать.
    pub fn try_open_confirm_cancel(&mut self) -> bool {
        if !self.actions_allowed || self.current_tab != Tab::Activity {
            return false;
        }
        let Some(idx) = self.table_state.selected() else {
            return false;
        };
        let Some(b) = self.visible_backend(idx) else {
            return false;
        };
        if b.is_self() {
            return false;
        }
        self.mode = Mode::ConfirmCancel(b.pid);
        true
    }

    /// Записать результат action'а. Вызывается, когда executor
    /// прислал свежий ActionResult.
    pub fn set_action_result(&mut self, result: ActionResult) {
        self.last_action_result = Some(result);
    }

    /// Хоткей `K` (Activity, actions_allowed): открыть confirm-terminate
    /// модалку. Те же предохранители, что и для cancel: только Activity, только
    /// не-self-row.
    pub fn try_open_confirm_terminate(&mut self) -> bool {
        if !self.actions_allowed || self.current_tab != Tab::Activity {
            return false;
        }
        let Some(idx) = self.table_state.selected() else {
            return false;
        };
        let Some(b) = self.visible_backend(idx) else {
            return false;
        };
        if b.is_self() {
            return false;
        }
        self.mode = Mode::ConfirmTerminate(b.pid, String::new());
        true
    }

    /// Добавить символ в текст подтверждения terminate. No-op в других режимах.
    pub fn terminate_input_push(&mut self, c: char) {
        if let Mode::ConfirmTerminate(_, text) = &mut self.mode {
            text.push(c);
        }
    }

    /// Удалить последний символ. No-op в других режимах.
    pub fn terminate_input_backspace(&mut self) {
        if let Mode::ConfirmTerminate(_, text) = &mut self.mode {
            text.pop();
        }
    }

    /// Если в `Mode::ConfirmTerminate` и набрано ровно `yes` — закрыть модалку
    /// и вернуть pid (caller отправит команду executor'у). Иначе — None.
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

    /// Войти в режим редактирования фильтра. Существующий input/regex
    /// сохраняется — можно «дописывать» к старому фильтру.
    pub fn enter_filter_mode(&mut self) {
        self.mode = Mode::Filter;
    }

    /// Хоткей `s`: следующая колонка сортировки (циклом).
    pub fn cycle_sort_column(&mut self) {
        self.sort.by = self.sort.by.next();
        self.recompute_filtered();
    }

    /// Хоткей `S`: переключить направление (asc ↔ desc).
    pub fn toggle_sort_direction(&mut self) {
        self.sort.direction = self.sort.direction.flip();
        self.recompute_filtered();
    }

    /// Переключиться на конкретный таб (хоткеи 1/2/3/4).
    pub fn set_tab(&mut self, tab: Tab) {
        self.current_tab = tab;
    }

    /// Переключиться на следующий таб циклом (хоткей `Tab`).
    pub fn next_tab(&mut self) {
        let next = (self.current_tab.index() + 1) % Tab::all().len();
        self.current_tab = Tab::from_index(next).unwrap();
    }

    /// Выйти из Filter mode. `commit=true` (Enter) — фильтр остаётся;
    /// `commit=false` (Esc) — сбрасываем фильтр.
    pub fn exit_filter_mode(&mut self, commit: bool) {
        if !commit {
            self.filter.clear();
            self.recompute_filtered();
        }
        self.mode = Mode::Normal;
    }

    /// Транслировать `KeyEvent` → `InputRequest` и пробросить в `tui-input`.
    /// Если ввод изменился (Some(StateChanged)) — пересобираем regex и `filtered`.
    ///
    /// Своя трансляция (вместо tui-input'овской `crossterm`-фичи) нужна потому,
    /// что та версия фичи внутри tui-input 0.10 завязана на crossterm 0.28,
    /// а у нас 0.29 — два несовместимых типа `Event` в дереве зависимостей.
    /// Заодно: явный контроль над тем, какие хоткеи поддерживаем.
    pub fn handle_filter_input(&mut self, key: KeyEvent) {
        let Some(req) = key_to_request(key) else {
            return;
        };
        if self.filter.input.handle(req).is_some() {
            self.filter.rebuild_regex();
            self.recompute_filtered();
        }
    }
}

/// Ring-буферы для sparkline'ов в шапке + последний снапшот для отображения
/// текущего значения. Длина буферов = `STATS_HISTORY_LEN`; при переполнении
/// `pop_front` сдвигает «горизонт» вправо (старые данные выпадают слева).
///
/// Несмотря на то, что Stats — это struct, мы храним каждое поле в своём
/// VecDeque (а не VecDeque<Stats>): для render'а sparkline нужен срез
/// одного метрика, а не кортежа всех. Memory cost минимальный.
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
    /// Push нового значения в каждое поле + drop старого если len > capacity.
    /// `pop_front` на VecDeque — амортизированно O(1); никаких relayout'ов
    /// массива, как у Vec.
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

/// Сравнить два backend'а по выбранной колонке. Без учёта direction —
/// asc/desc применяется выше через `Ordering::reverse`.
///
/// Используем tuple/Option Ord, где это возможно: `Option<String>::cmp`
/// имеет нужное поведение (None < Some, лексикографически), и нет лишних
/// String-аллокаций. Для wait сравниваем как `(type, event)`-кортеж.
fn compare_backends(a: &Backend, b: &Backend, by: SortBy, now: chrono::DateTime<Utc>) -> Ordering {
    match by {
        SortBy::Pid => a.pid.cmp(&b.pid),
        SortBy::User => a.usename.cmp(&b.usename),
        SortBy::State => a.state.cmp(&b.state),
        SortBy::Wait => (a.wait_event_type.as_deref(), a.wait_event.as_deref())
            .cmp(&(b.wait_event_type.as_deref(), b.wait_event.as_deref())),
        SortBy::Duration => {
            // `Option<TimeDelta>::cmp` — None < Some. Asc по duration =
            // самые свежие/idle-без-query сначала; desc = самые долгие сверху
            // (типичная задача мониторинга).
            let da = a.query_start.map(|s| now - s);
            let db = b.query_start.map(|s| now - s);
            da.cmp(&db)
        }
        SortBy::Query => a.query.cmp(&b.query),
    }
}

/// Маппинг crossterm-клавиш на `tui_input::InputRequest`. Поддержанный
/// набор — emacs/readline-стиль:
/// - буквы/цифры → InsertChar
/// - Backspace/Delete → удаление одного символа
/// - стрелки/Home/End → перемещение курсора
/// - Ctrl+A/E → начало/конец строки
/// - Ctrl+U → очистить строку
/// - Ctrl+W → удалить слово назад
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
