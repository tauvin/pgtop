//! Tables tab view: per-table bloat / scan stats from `pg_stat_user_tables`.

use chrono::{DateTime, Utc};
use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::{Modifier, Style, Stylize},
    text::{Line, Text},
    widgets::{Paragraph, Row, Table, TableState, Wrap},
};

use crate::{app::App, db::TableStat};

pub fn render_tables(frame: &mut Frame, area: Rect, app: &mut App, now: DateTime<Utc>) {
    let conn = app.active_mut();
    if conn.tables.is_empty() {
        render_empty(frame, area);
    } else {
        render_table(frame, area, &conn.tables, &mut conn.tables_table_state, now);
    }
}

fn render_empty(frame: &mut Frame, area: Rect) {
    let text = Text::from(vec![
        Line::from(""),
        Line::from(vec!["  ".into(), "Loading…".bold()]),
        Line::from(""),
        Line::from("  pg_stat_user_tables is empty: no user tables in the"),
        Line::from("  current database, or the role lacks pg_read_all_stats"),
        Line::from("  / pg_monitor."),
    ]);

    let para = Paragraph::new(text).wrap(Wrap { trim: false });
    frame.render_widget(para, area);
}

fn render_table(
    frame: &mut Frame,
    area: Rect,
    tables: &[TableStat],
    table_state: &mut TableState,
    now: DateTime<Utc>,
) {
    let header_style = Style::new().add_modifier(Modifier::BOLD);
    let header = Row::new([
        "table", "live", "dead", "dead %", "vacuum", "analyze", "seq", "idx",
    ])
    .style(header_style);

    let widths = [
        Constraint::Min(28),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(7),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(8),
    ];

    let rows: Vec<Row<'static>> = tables.iter().map(|t| table_to_row(t, now)).collect();

    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(Style::new().reversed());

    frame.render_stateful_widget(table, area, table_state);
}

fn table_to_row(t: &TableStat, now: DateTime<Utc>) -> Row<'static> {
    Row::new([
        format!("{}.{}", t.schemaname, t.relname),
        format_count(t.n_live_tup),
        format_count(t.n_dead_tup),
        match t.dead_pct() {
            Some(pct) => format!("{pct:.1}%"),
            None => "—".to_string(),
        },
        format_ago(t.last_vacuum, now),
        format_ago(t.last_analyze, now),
        format_count(t.seq_scan),
        format_count(t.idx_scan),
    ])
}

fn format_count(n: i64) -> String {
    let n = n.max(0);
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 10_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Compact "time ago" formatter: "2m" / "3h" / "5d" / "—".
fn format_ago(then: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    let Some(then) = then else {
        return "—".to_string();
    };
    let secs = (now - then).num_seconds();
    if secs < 0 {
        return "0s".to_string();
    }
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3_600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3_600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t() -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000, 0).unwrap()
    }

    #[test]
    fn format_ago_em_dash_for_none() {
        assert_eq!(format_ago(None, t()), "—");
    }

    #[test]
    fn format_ago_picks_unit() {
        let now = t();
        assert_eq!(
            format_ago(Some(now - chrono::Duration::seconds(30)), now),
            "30s"
        );
        assert_eq!(
            format_ago(Some(now - chrono::Duration::seconds(120)), now),
            "2m"
        );
        assert_eq!(
            format_ago(Some(now - chrono::Duration::seconds(7200)), now),
            "2h"
        );
        assert_eq!(
            format_ago(Some(now - chrono::Duration::seconds(2 * 86_400)), now),
            "2d"
        );
    }

    #[test]
    fn format_ago_clamps_future_to_zero() {
        let now = t();
        let future = now + chrono::Duration::seconds(60);
        assert_eq!(format_ago(Some(future), now), "0s");
    }

    #[test]
    fn dead_pct_handles_empty_relation() {
        let s = TableStat {
            schemaname: "public".into(),
            relname: "x".into(),
            n_live_tup: 0,
            n_dead_tup: 0,
            last_vacuum: None,
            last_analyze: None,
            seq_scan: 0,
            idx_scan: 0,
        };
        assert!(s.dead_pct().is_none());
    }

    #[test]
    fn dead_pct_compute() {
        let s = TableStat {
            schemaname: "public".into(),
            relname: "x".into(),
            n_live_tup: 80,
            n_dead_tup: 20,
            last_vacuum: None,
            last_analyze: None,
            seq_scan: 0,
            idx_scan: 0,
        };
        assert_eq!(s.dead_pct(), Some(20.0));
    }
}
