//! Waits tab view: histogram of `(wait_event_type, wait_event)` pairs over
//! the latest activity snapshot. Lightweight sampling — no extra SQL.

use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::{Modifier, Style, Stylize},
    text::{Line, Text},
    widgets::{Paragraph, Row, Table, TableState, Wrap},
};

use crate::app::{App, WaitRow};

pub fn render_waits(frame: &mut Frame, area: Rect, app: &mut App) {
    let conn = app.active_mut();
    if conn.waits.is_empty() {
        render_empty(frame, area);
    } else {
        render_table(frame, area, &conn.waits, &mut conn.waits_table_state);
    }
}

fn render_empty(frame: &mut Frame, area: Rect) {
    let text = Text::from(vec![
        Line::from(""),
        Line::from(vec!["  ".into(), "No waits in the latest snapshot.".bold()]),
        Line::from(""),
        Line::from("  Waits are aggregated from pg_stat_activity at activity"),
        Line::from("  poll rate (default 1 Hz). An empty histogram means every"),
        Line::from("  backend is either idle or running CPU-bound — pgtop's"),
        Line::from("  sampling is sparse, so very fast waits may be missed."),
    ]);

    let para = Paragraph::new(text).wrap(Wrap { trim: false });
    frame.render_widget(para, area);
}

fn render_table(frame: &mut Frame, area: Rect, waits: &[WaitRow], table_state: &mut TableState) {
    let total: u32 = waits.iter().map(|w| w.count).sum();
    let header_style = Style::new().add_modifier(Modifier::BOLD);
    let header = Row::new(["type", "event", "count", "share"]).style(header_style);

    let widths = [
        Constraint::Length(20),
        Constraint::Min(24),
        Constraint::Length(8),
        Constraint::Length(8),
    ];

    let rows: Vec<Row<'static>> = waits.iter().map(|w| wait_to_row(w, total)).collect();

    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(Style::new().reversed());

    frame.render_stateful_widget(table, area, table_state);
}

fn wait_to_row(w: &WaitRow, total: u32) -> Row<'static> {
    let pct = if total > 0 {
        100.0 * w.count as f64 / total as f64
    } else {
        0.0
    };
    Row::new([
        w.wait_event_type.clone(),
        w.wait_event.clone(),
        w.count.to_string(),
        format!("{pct:.1}%"),
    ])
}
