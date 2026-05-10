//! Locks tab view: таблица блокировок с цветовой пометкой waiting-локов.

use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::{Color, Modifier, Style},
    widgets::{Row, Table},
};

use crate::{app::App, db::Lock};

/// Контент Locks таба: таблица из `app.locks`, индексируемая `app.locks_table_state`.
pub fn render_locks(frame: &mut Frame, area: Rect, app: &mut App) {
    let header_style = Style::new().add_modifier(Modifier::BOLD);
    let header = Row::new(["pid", "type", "mode", "✓", "object"]).style(header_style);

    let widths = [
        Constraint::Length(7),
        Constraint::Length(15),
        Constraint::Length(22),
        Constraint::Length(3),
        Constraint::Min(0),
    ];

    let rows: Vec<Row<'static>> = app.locks.iter().map(lock_to_row).collect();

    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(Style::new().reversed());

    frame.render_stateful_widget(table, area, &mut app.locks_table_state);
}

/// `Lock` → `Row<'static>`. Waiting-локи (granted=false) подсвечиваем красным —
/// это типичный сигнал contention'а для мониторинга.
fn lock_to_row(l: &Lock) -> Row<'static> {
    let granted_marker = if l.granted { "✓" } else { "⏳" };

    let row = Row::new([
        l.pid.to_string(),
        l.locktype.clone(),
        l.mode.clone(),
        granted_marker.to_string(),
        l.object.clone().unwrap_or_else(em_dash),
    ]);

    if l.granted {
        row
    } else {
        row.style(Style::new().fg(Color::Red))
    }
}

fn em_dash() -> String {
    "—".to_string()
}
