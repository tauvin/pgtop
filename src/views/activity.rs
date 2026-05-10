//! Activity tab view: backends table with state/duration colouring and a
//! sort indicator in the header.

use chrono::{DateTime, Utc};
use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::{Modifier, Style},
    widgets::{Row, Table},
};

use crate::{
    app::{App, Sort, SortBy},
    db::Backend,
    theme::Theme,
};

/// Render the Activity tab — a stateful table fed by `ConnectionState`.
pub fn render_activity(frame: &mut Frame, area: Rect, app: &mut App) {
    let theme = app.theme;
    let conn = app.active_mut();
    let header = build_header_row(conn.sort);

    let widths = [
        Constraint::Length(7),
        Constraint::Length(14),
        Constraint::Length(20),
        Constraint::Length(15),
        Constraint::Length(10),
        Constraint::Min(0),
    ];

    let now = Utc::now();
    let rows: Vec<Row<'static>> = conn
        .visible_backends()
        .map(|b| backend_to_row(b, now, theme))
        .collect();

    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(Style::new().reversed());

    frame.render_stateful_widget(table, area, &mut conn.table_state);
}

fn build_header_row(sort: Sort) -> Row<'static> {
    const COLUMNS: [SortBy; 6] = [
        SortBy::Pid,
        SortBy::User,
        SortBy::State,
        SortBy::Wait,
        SortBy::Duration,
        SortBy::Query,
    ];

    let cells: Vec<String> = COLUMNS
        .iter()
        .map(|&col| {
            if col == sort.by {
                format!("{} {}", col.label(), sort.direction.arrow())
            } else {
                col.label().to_string()
            }
        })
        .collect();

    Row::new(cells).style(Style::new().add_modifier(Modifier::BOLD))
}

fn backend_to_row(b: &Backend, now: DateTime<Utc>, theme: Theme) -> Row<'static> {
    Row::new([
        b.pid.to_string(),
        b.usename.clone().unwrap_or_else(em_dash),
        b.state.clone().unwrap_or_else(em_dash),
        format_wait(b),
        format_duration(b.query_start, now),
        format_query(b.query.as_deref()),
    ])
    .style(row_style(b, now, theme))
}

fn row_style(b: &Backend, now: DateTime<Utc>, theme: Theme) -> Style {
    const LONG_QUERY_THRESHOLD_SECS: i64 = 10;

    if b.is_self() {
        return Style::new().fg(theme.muted);
    }

    let state = b.state.as_deref();

    if state == Some("active") {
        let long = b
            .query_start
            .map(|s| (now - s).num_seconds() > LONG_QUERY_THRESHOLD_SECS)
            .unwrap_or(false);
        return if long {
            Style::new().fg(theme.danger)
        } else {
            Style::new().fg(theme.success)
        };
    }

    if state.is_some_and(|s| s.starts_with("idle in transaction")) {
        return Style::new().fg(theme.warning);
    }

    Style::default()
}

fn format_wait(b: &Backend) -> String {
    match (&b.wait_event_type, &b.wait_event) {
        (Some(t), Some(e)) => format!("{t}: {e}"),
        (Some(t), None) => t.clone(),
        (None, Some(e)) => e.clone(),
        (None, None) => em_dash(),
    }
}

fn format_duration(query_start: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    let Some(start) = query_start else {
        return em_dash();
    };
    let total_secs = (now - start).num_seconds().max(0);
    let h = total_secs / 3600;
    let m = (total_secs % 3600) / 60;
    let s = total_secs % 60;
    format!("{h}:{m:02}:{s:02}")
}

fn format_query(query: Option<&str>) -> String {
    let Some(q) = query else {
        return em_dash();
    };
    let one_line: String = q.split_whitespace().collect::<Vec<_>>().join(" ");
    match one_line.char_indices().nth(60) {
        Some((cutoff, _)) => format!("{}…", &one_line[..cutoff]),
        None => one_line,
    }
}

fn em_dash() -> String {
    "—".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn backend() -> Backend {
        Backend {
            pid: 0,
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

    #[test]
    fn format_wait_renders_em_dash_for_idle_backend() {
        assert_eq!(format_wait(&backend()), "—");
    }

    #[test]
    fn format_wait_combines_type_and_event() {
        let mut b = backend();
        b.wait_event_type = Some("Lock".to_string());
        b.wait_event = Some("relation".to_string());
        assert_eq!(format_wait(&b), "Lock: relation");
    }

    #[test]
    fn format_wait_falls_back_to_either_field() {
        let mut b = backend();
        b.wait_event_type = Some("Client".to_string());
        assert_eq!(format_wait(&b), "Client");

        let mut b = backend();
        b.wait_event = Some("ClientRead".to_string());
        assert_eq!(format_wait(&b), "ClientRead");
    }

    #[test]
    fn format_duration_renders_em_dash_for_no_query() {
        let now = Utc.timestamp_opt(1_000, 0).unwrap();
        assert_eq!(format_duration(None, now), "—");
    }

    #[test]
    fn format_duration_pads_minutes_and_seconds() {
        let now = Utc.timestamp_opt(3725, 0).unwrap();
        let start = Utc.timestamp_opt(0, 0).unwrap();
        assert_eq!(format_duration(Some(start), now), "1:02:05");
    }

    #[test]
    fn format_duration_clamps_negative_clock_skew_to_zero() {
        let now = Utc.timestamp_opt(0, 0).unwrap();
        let start = Utc.timestamp_opt(60, 0).unwrap();
        assert_eq!(format_duration(Some(start), now), "0:00:00");
    }

    #[test]
    fn format_query_collapses_whitespace_and_joins_lines() {
        let q = "SELECT  *\n  FROM   users\n  WHERE id = 1";
        assert_eq!(format_query(Some(q)), "SELECT * FROM users WHERE id = 1");
    }

    #[test]
    fn format_query_truncates_to_60_chars_with_ellipsis() {
        let q = "x".repeat(100);
        let out = format_query(Some(&q));
        let chars: Vec<char> = out.chars().collect();
        assert_eq!(chars.len(), 61);
        assert_eq!(chars[60], '…');
    }

    #[test]
    fn format_query_handles_none() {
        assert_eq!(format_query(None), "—");
    }
}
