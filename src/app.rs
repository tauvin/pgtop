//! Состояние приложения, которое переживает между кадрами.
//!
//! Каждый кадр UI получает `&mut App`. Обновления данных от collector'а заходят
//! в `App::set_backends`. На Phase 4 block C добавилось поле `filter`
//! (regex-фильтр по тексту query) и `filtered` (предвычисленные индексы
//! видимых backend'ов).

use std::cmp::Ordering;

use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::TableState;
use regex::{Regex, RegexBuilder};
use tui_input::{Input, InputRequest};

use crate::db::Backend;

/// Модальные состояния UI.
///
/// `Detail(pid)` хранит **pid**, а не индекс: список перетасуется, индекс
/// «уплывёт», а pid стабилен. Если pid исчезнет, `set_backends` авто-возвращает
/// в `Normal`.
///
/// `Filter` — режим редактирования фильтра. Сам текст и regex живут в
/// `App.filter`, чтобы переживать выход из режима (Enter коммитит, Esc отменяет).
#[derive(Debug, Clone)]
pub enum Mode {
    Normal,
    Detail(i32),
    Filter,
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
    pub mode: Mode,
    pub filter: Filter,
    pub sort: Sort,
}

impl App {
    pub fn new() -> Self {
        Self {
            backends: Vec::new(),
            filtered: Vec::new(),
            table_state: TableState::default(),
            mode: Mode::Normal,
            filter: Filter::default(),
            sort: Sort::default(),
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

    /// Обновить snapshot. Заодно пересчитываем filtered и закрываем
    /// Detail-модалку, если её pid исчез.
    pub fn set_backends(&mut self, backends: Vec<Backend>) {
        self.backends = backends;
        self.recompute_filtered();

        if let Mode::Detail(pid) = &self.mode
            && !self.backends.iter().any(|b| b.pid == *pid)
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

    pub fn select_previous(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        let i = self
            .table_state
            .selected()
            .map_or(0, |i| i.saturating_sub(1));
        self.table_state.select(Some(i));
    }

    pub fn select_next(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        let max = self.filtered.len() - 1;
        let i = self.table_state.selected().map_or(0, |i| (i + 1).min(max));
        self.table_state.select(Some(i));
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
