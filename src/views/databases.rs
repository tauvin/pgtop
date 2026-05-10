//! Databases tab view: per-database stats from `pg_stat_database`.

use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::{Modifier, Style, Stylize},
    text::{Line, Text},
    widgets::{Paragraph, Row, Table, TableState, Wrap},
};

use crate::{app::App, db::DatabaseStat};

pub fn render_databases(frame: &mut Frame, area: Rect, app: &mut App) {
    let conn = app.active_mut();
    if conn.databases.is_empty() {
        render_empty(frame, area);
    } else {
        render_table(
            frame,
            area,
            &conn.databases,
            &mut conn.databases_table_state,
        );
    }
}

fn render_empty(frame: &mut Frame, area: Rect) {
    let text = Text::from(vec![
        Line::from(""),
        Line::from(vec!["  ".into(), "Loading…".bold()]),
        Line::from(""),
        Line::from("  pg_stat_database has not been polled yet, or the role"),
        Line::from("  cannot read it. Grant pg_read_all_stats (or pg_monitor"),
        Line::from("  on PG ≥ 10) to the connecting role to populate this view."),
    ]);

    let para = Paragraph::new(text).wrap(Wrap { trim: false });
    frame.render_widget(para, area);
}

fn render_table(
    frame: &mut Frame,
    area: Rect,
    databases: &[DatabaseStat],
    table_state: &mut TableState,
) {
    let header_style = Style::new().add_modifier(Modifier::BOLD);
    let header = Row::new([
        "database",
        "conns",
        "commits",
        "rollbacks",
        "cache hit",
        "temp",
        "deadlocks",
    ])
    .style(header_style);

    let widths = [
        Constraint::Min(20),
        Constraint::Length(7),
        Constraint::Length(13),
        Constraint::Length(11),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(10),
    ];

    let rows: Vec<Row<'static>> = databases.iter().map(db_to_row).collect();

    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(Style::new().reversed());

    frame.render_stateful_widget(table, area, table_state);
}

fn db_to_row(d: &DatabaseStat) -> Row<'static> {
    Row::new([
        d.datname.clone(),
        d.numbackends.to_string(),
        format_count(d.xact_commit),
        format_count(d.xact_rollback),
        format!("{:.1}%", d.cache_hit_pct()),
        format_bytes(d.temp_bytes),
        format_count(d.deadlocks),
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

fn format_bytes(n: i64) -> String {
    let n = n.max(0) as u64;
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if n >= GB {
        format!("{:.1} GB", n as f64 / GB as f64)
    } else if n >= MB {
        format!("{:.1} MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.1} KB", n as f64 / KB as f64)
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_count_uses_plain_under_10k() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(9_999), "9999");
    }

    #[test]
    fn format_count_uses_si_suffixes_for_large_values() {
        assert_eq!(format_count(10_000), "10.0K");
        assert_eq!(format_count(1_500_000), "1.5M");
        assert_eq!(format_count(2_500_000_000), "2.5B");
    }

    #[test]
    fn format_count_clamps_negative() {
        assert_eq!(format_count(-1), "0");
    }

    #[test]
    fn format_bytes_picks_unit() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2 * 1024), "2.0 KB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn cache_hit_pct_handles_idle_database() {
        let d = DatabaseStat {
            datname: "x".into(),
            numbackends: 0,
            xact_commit: 0,
            xact_rollback: 0,
            blks_hit: 0,
            blks_read: 0,
            temp_bytes: 0,
            deadlocks: 0,
        };
        assert_eq!(d.cache_hit_pct(), 100.0);
    }

    #[test]
    fn cache_hit_pct_compute() {
        let d = DatabaseStat {
            datname: "x".into(),
            numbackends: 0,
            xact_commit: 0,
            xact_rollback: 0,
            blks_hit: 99,
            blks_read: 1,
            temp_bytes: 0,
            deadlocks: 0,
        };
        assert_eq!(d.cache_hit_pct(), 99.0);
    }
}
