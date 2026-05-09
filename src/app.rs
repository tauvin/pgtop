//! Состояние приложения, которое переживает между кадрами.
//!
//! Каждый кадр UI получает `&mut App`. Обновления данных от collector'а заходят
//! в `App::set_backends` (с авто-clamp'ом выделения под новый размер списка).
//! В Phase 4 здесь добавится `mode: Mode`, `filter: String` и т.п.

use ratatui::widgets::TableState;

use crate::db::Backend;

/// Корневое состояние приложения.
pub struct App {
    /// Последний снапшот `pg_stat_activity`. Меняется только через `set_backends`,
    /// чтобы вместе с заменой данных всегда корректировалось выделение.
    pub backends: Vec<Backend>,

    /// Состояние Table-виджета (selected + offset). Виджет — stateless, между
    /// кадрами state живёт здесь.
    pub table_state: TableState,
}

impl App {
    pub fn new() -> Self {
        Self {
            backends: Vec::new(),
            // Селекшен изначально None — пока не пришёл первый snapshot,
            // и подсвечивать в пустой таблице нечего.
            table_state: TableState::default(),
        }
    }

    /// Обновить snapshot. Заодно привязки выделения к индексам:
    /// - если список опустел → selected = None;
    /// - если selected ушёл за len → подвинуть на последнюю строку;
    /// - если selected был None и пришли данные → выбрать первую строку;
    /// - иначе оставить как есть.
    pub fn set_backends(&mut self, backends: Vec<Backend>) {
        self.backends = backends;
        let len = self.backends.len();
        match self.table_state.selected() {
            _ if len == 0 => self.table_state.select(None),
            Some(i) if i >= len => self.table_state.select(Some(len - 1)),
            None => self.table_state.select(Some(0)),
            Some(_) => {} // selection still valid
        }
    }

    /// Сдвиг выделения на строку выше; сатурируется на 0.
    pub fn select_previous(&mut self) {
        if self.backends.is_empty() {
            return;
        }
        let i = self
            .table_state
            .selected()
            .map_or(0, |i| i.saturating_sub(1));
        self.table_state.select(Some(i));
    }

    /// Сдвиг выделения на строку ниже; сатурируется на len-1.
    pub fn select_next(&mut self) {
        if self.backends.is_empty() {
            return;
        }
        let max = self.backends.len() - 1;
        let i = self.table_state.selected().map_or(0, |i| (i + 1).min(max));
        self.table_state.select(Some(i));
    }

    /// Enter на выбранной строке. На Phase 3 — заглушка: точка расширения
    /// для Phase 4, где это переключит `Mode` в `Detail(pid)` и откроет
    /// модалку с полным текстом запроса, `wait_event`, `backend_xmin` и т.п.
    /// Делаем `no-op`-метод заранее, чтобы маршрутизация хоткея в main была
    /// финальной формы — Phase 4 трогает только тело метода, не event loop.
    pub fn on_enter(&mut self) {
        // TODO(phase 4): self.mode = Mode::Detail(selected_pid)
    }
}
