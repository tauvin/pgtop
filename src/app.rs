//! Состояние приложения, которое переживает между кадрами.
//!
//! Каждый кадр UI получает `&mut App`. На task 5 здесь — только захардкоженные
//! строки и `TableState`. В Phase 3 поле `rows` заменится на `backends:
//! Vec<Backend>` (от collector'а через watch-канал), в Phase 4 добавятся
//! `mode: Mode`, `filter: String` и т.п.

use ratatui::widgets::TableState;

/// Псевдоним для одной строки mock-данных. На task 5 столбцов 6:
/// pid, user, state, wait, duration, query.
pub type MockRow = [&'static str; 6];

/// Корневое состояние приложения.
pub struct App {
    /// Захардкоженные строки task 4-5; в Phase 3 заменится на `Vec<Backend>`.
    pub rows: Vec<MockRow>,

    /// Состояние `Table`-виджета: индекс выделенной строки и offset скролла.
    /// Виджет stateless (рисуется на каждом кадре), а это поле — то, что
    /// между кадрами помнит «какую строку юзер выбрал».
    pub table_state: TableState,
}

impl App {
    pub fn new() -> Self {
        let mut table_state = TableState::default();
        // Стартуем с выбранной первой строкой — иначе пользователю кажется,
        // что pgtop «ничего не выделяет», пока он не нажмёт Down первый раз.
        table_state.select(Some(0));
        Self {
            rows: mock_rows(),
            table_state,
        }
    }

    /// Сдвиг выделения на строку выше.
    ///
    /// Сатурируется на 0 (нет wrap-around). Делаем вручную через
    /// `selected().map_or(0, |i| i.saturating_sub(1))`, потому что
    /// у ratatui-метода `select_previous` своя политика, и проще
    /// контролировать самим.
    pub fn select_previous(&mut self) {
        let i = self
            .table_state
            .selected()
            .map_or(0, |i| i.saturating_sub(1));
        self.table_state.select(Some(i));
    }

    /// Сдвиг выделения на строку ниже; сатурируется на `rows.len() - 1`.
    pub fn select_next(&mut self) {
        let max = self.rows.len().saturating_sub(1);
        let i = self.table_state.selected().map_or(0, |i| (i + 1).min(max));
        self.table_state.select(Some(i));
    }
}

/// Захардкоженные строки task 4-5: смесь типичных backend'ов в Postgres.
/// В Phase 3 функцию заменит поток данных от collector'а через watch-канал.
///
/// `#[rustfmt::skip]` сохраняет «табличный» вид: rustfmt бы разбил длинные
/// строки на multi-line и сломал колоночное выравнивание.
#[rustfmt::skip]
fn mock_rows() -> Vec<MockRow> {
    vec![
        ["12345", "postgres",   "active",              "—",            "0:00:01", "SELECT * FROM pg_stat_activity"],
        ["12346", "pgtop",      "idle",                "—",            "0:01:23", "—"],
        ["12347", "app",        "active",              "Lock: tuple",  "0:00:42", "UPDATE orders SET status = $1 WHERE id = $2"],
        ["12348", "app",        "idle in transaction", "—",            "0:02:15", "BEGIN"],
        ["12349", "replicator", "active",              "WALWriteLock", "0:00:05", "SELECT pg_current_wal_lsn()"],
    ]
}
