//! Activity tab view: backends table with state/duration colouring and a
//! sort indicator in the header.

use std::borrow::Cow;

use chrono::{DateTime, Utc};
use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::{Modifier, Style},
    widgets::{Cell, Row, Table},
};

use crate::{
    app::{App, Sort, SortBy},
    db::Backend,
    theme::Theme,
};

const EM_DASH: &str = "—";

/// Render the Activity tab — a stateful table fed by `ConnectionState`.
pub fn render_activity(frame: &mut Frame, area: Rect, app: &mut App, now: DateTime<Utc>) {
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

    let slow_threshold_secs = conn.slow_query_threshold.as_secs() as i64;
    // Disjoint-field borrows: backends/filtered immutably + table_state
    // mutably is fine because they're different fields of the same struct.
    // visible_backends() takes &self of the whole struct and would block
    // the &mut on table_state, hence the inline iteration.
    let backends = &conn.backends;
    let rows: Vec<Row<'_>> = conn
        .filtered
        .iter()
        .filter_map(|&i| backends.get(i))
        .map(|b| backend_to_row(b, now, theme, slow_threshold_secs))
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

fn backend_to_row<'a>(
    b: &'a Backend,
    now: DateTime<Utc>,
    theme: Theme,
    slow_threshold_secs: i64,
) -> Row<'a> {
    let cells: [Cell<'a>; 6] = [
        Cell::from(b.pid.to_string()),
        Cell::from(borrow_or_dash(b.usename.as_deref())),
        Cell::from(borrow_or_dash(b.state.as_deref())),
        Cell::from(format_wait(b)),
        Cell::from(format_duration(b.query_start, now)),
        Cell::from(format_query(b.query.as_deref())),
    ];
    Row::new(cells).style(row_style(b, now, theme, slow_threshold_secs))
}

fn borrow_or_dash(field: Option<&str>) -> Cow<'_, str> {
    field.map_or(Cow::Borrowed(EM_DASH), Cow::Borrowed)
}

fn row_style(b: &Backend, now: DateTime<Utc>, theme: Theme, slow_threshold_secs: i64) -> Style {
    if b.is_self() {
        return Style::new().fg(theme.muted);
    }

    let state = b.state.as_deref();

    if state == Some("active") {
        let slow = b
            .query_start
            .map(|s| (now - s).num_seconds() > slow_threshold_secs)
            .unwrap_or(false);
        return if slow {
            Style::new().fg(theme.danger).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme.success)
        };
    }

    if state.is_some_and(|s| s.starts_with("idle in transaction")) {
        return Style::new().fg(theme.warning);
    }

    Style::default()
}

fn format_wait(b: &Backend) -> Cow<'_, str> {
    match (b.wait_event_type.as_deref(), b.wait_event.as_deref()) {
        (Some(t), Some(e)) => Cow::Owned(format!("{t}: {e}")),
        (Some(t), None) => Cow::Borrowed(t),
        (None, Some(e)) => Cow::Borrowed(e),
        (None, None) => Cow::Borrowed(EM_DASH),
    }
}

fn format_duration(query_start: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Cow<'static, str> {
    let Some(start) = query_start else {
        return Cow::Borrowed(EM_DASH);
    };
    let total_secs = (now - start).num_seconds().max(0);
    let h = total_secs / 3600;
    let m = (total_secs % 3600) / 60;
    let s = total_secs % 60;
    Cow::Owned(format!("{h}:{m:02}:{s:02}"))
}

fn format_query(query: Option<&str>) -> Cow<'_, str> {
    let Some(q) = query else {
        return Cow::Borrowed(EM_DASH);
    };
    let one_line: String = q.split_whitespace().collect::<Vec<_>>().join(" ");
    match one_line.char_indices().nth(60) {
        Some((cutoff, _)) => Cow::Owned(format!("{}…", &one_line[..cutoff])),
        None => Cow::Owned(one_line),
    }
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
