//! Replication tab view: streaming replicas table or an empty-state hint.

use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::{Modifier, Style, Stylize},
    text::{Line, Text},
    widgets::{Paragraph, Row, Table, TableState, Wrap},
};

use crate::{app::App, db::Replica};

pub fn render_replication(frame: &mut Frame, area: Rect, app: &mut App) {
    let conn = app.active_mut();
    if conn.replication.is_empty() {
        render_empty(frame, area);
    } else {
        render_table(
            frame,
            area,
            &conn.replication,
            &mut conn.replication_table_state,
        );
    }
}

fn render_empty(frame: &mut Frame, area: Rect) {
    let text = Text::from(vec![
        Line::from(""),
        Line::from(vec!["  ".into(), "No active replicas.".bold()]),
        Line::from(""),
        Line::from("  pg_stat_replication shows clients connected via streaming"),
        Line::from("  replication: standby replicas, pg_basebackup sessions, etc."),
        Line::from("  This table is empty by default — entries appear when a"),
        Line::from("  replication client connects to this server."),
    ]);

    let para = Paragraph::new(text).wrap(Wrap { trim: false });
    frame.render_widget(para, area);
}

fn render_table(frame: &mut Frame, area: Rect, replicas: &[Replica], table_state: &mut TableState) {
    let header_style = Style::new().add_modifier(Modifier::BOLD);
    let header = Row::new([
        "pid",
        "application",
        "state",
        "sync",
        "replay lag",
        "replay lsn",
    ])
    .style(header_style);

    let widths = [
        Constraint::Length(7),
        Constraint::Length(18),
        Constraint::Length(14),
        Constraint::Length(10),
        Constraint::Length(12),
        Constraint::Min(0),
    ];

    let rows: Vec<Row<'static>> = replicas.iter().map(replica_to_row).collect();

    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(Style::new().reversed());

    frame.render_stateful_widget(table, area, table_state);
}

fn replica_to_row(r: &Replica) -> Row<'static> {
    Row::new([
        r.pid.to_string(),
        r.application_name.clone().unwrap_or_else(em_dash),
        r.state.clone().unwrap_or_else(em_dash),
        r.sync_state.clone().unwrap_or_else(em_dash),
        format_lag(r.replay_lag_secs),
        r.replay_lsn.clone().unwrap_or_else(em_dash),
    ])
}

fn format_lag(secs: Option<f64>) -> String {
    match secs {
        Some(s) => format!("{s:.1}s"),
        None => em_dash(),
    }
}

fn em_dash() -> String {
    "—".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_lag_renders_em_dash_for_none() {
        assert_eq!(format_lag(None), "—");
    }

    #[test]
    fn format_lag_renders_one_decimal() {
        assert_eq!(format_lag(Some(0.0)), "0.0s");
        assert_eq!(format_lag(Some(1.234)), "1.2s");
        assert_eq!(format_lag(Some(60.0)), "60.0s");
    }
}
