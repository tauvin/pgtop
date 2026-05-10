//! Locks tab view: lock table with waiting locks highlighted.

use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::{Modifier, Style},
    widgets::{Row, Table},
};

use crate::{app::App, db::Lock, theme::Theme};

/// Render the Locks tab.
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

    let theme = app.theme;
    let rows: Vec<Row<'static>> = app
        .active()
        .locks
        .iter()
        .map(|l| lock_to_row(l, theme))
        .collect();

    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(Style::new().reversed());

    frame.render_stateful_widget(table, area, &mut app.active_mut().locks_table_state);
}

fn lock_to_row(l: &Lock, theme: Theme) -> Row<'static> {
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
        row.style(Style::new().fg(theme.danger))
    }
}

fn em_dash() -> String {
    "—".to_string()
}
