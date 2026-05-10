//! Top Queries tab view: либо таблица топ-запросов, либо инструкция
//! как поставить `pg_stat_statements`, либо «загружается» — в зависимости
//! от `TopQueriesSnapshot`.

use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::{Modifier, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{Paragraph, Row, Table, TableState, Wrap},
};

use crate::{
    app::App,
    db::{TopQueriesSnapshot, TopQuery},
};

pub fn render_top_queries(frame: &mut Frame, area: Rect, app: &mut App) {
    // Disjoint field borrows: `&app.top_queries` (immut) и
    // `&mut app.top_queries_table_state` (mut) — разные поля, компилятор
    // пропустит. Передавать целиком `&mut app` в render_available нельзя:
    // там бы уже жил immutable borrow от match'а.
    match &app.top_queries {
        TopQueriesSnapshot::Loading => render_loading(frame, area),
        TopQueriesSnapshot::ExtensionMissing => render_extension_missing(frame, area),
        TopQueriesSnapshot::Available(queries) => {
            render_available(frame, area, queries, &mut app.top_queries_table_state);
        }
    }
}

fn render_loading(frame: &mut Frame, area: Rect) {
    let para = Paragraph::new(Line::from(vec!["  ".into(), "Loading…".dim()]));
    frame.render_widget(para, area);
}

/// Helpful instructions с конкретными командами для установки расширения.
/// Полезнее чем просто «расширение не установлено» — пользователь сразу видит,
/// что нужно сделать.
fn render_extension_missing(frame: &mut Frame, area: Rect) {
    let text = Text::from(vec![
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            "pg_stat_statements".bold(),
            Span::raw(" extension is not installed."),
        ]),
        Line::from(""),
        Line::from("  This tab tracks normalized query statistics — calls, total"),
        Line::from("  time, mean time. To enable:"),
        Line::from(""),
        Line::from(vec![
            Span::raw("    "),
            "1.".bold(),
            Span::raw(" Add to postgresql.conf:"),
        ]),
        Line::from("       shared_preload_libraries = 'pg_stat_statements'"),
        Line::from(""),
        Line::from(vec![
            Span::raw("    "),
            "2.".bold(),
            Span::raw(" Restart Postgres."),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("    "),
            "3.".bold(),
            Span::raw(" In your database:"),
        ]),
        Line::from("       CREATE EXTENSION pg_stat_statements;"),
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            "For docker-compose:".dim(),
            Span::raw(" "),
            "set `command:` with `-c shared_preload_libraries=...`".dim(),
        ]),
        Line::from(vec![
            Span::raw("  "),
            "and use ".dim(),
            "/docker-entrypoint-initdb.d".dim().italic(),
            Span::raw(" "),
            "for CREATE EXTENSION on init.".dim(),
        ]),
    ]);

    let para = Paragraph::new(text).wrap(Wrap { trim: false });
    frame.render_widget(para, area);
}

fn render_available(
    frame: &mut Frame,
    area: Rect,
    queries: &[TopQuery],
    table_state: &mut TableState,
) {
    let header_style = Style::new().add_modifier(Modifier::BOLD);
    let header = Row::new(["calls", "total ms", "mean ms", "rows", "query"]).style(header_style);

    let widths = [
        Constraint::Length(10),
        Constraint::Length(14),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Min(0),
    ];

    let rows: Vec<Row<'static>> = queries.iter().map(top_query_to_row).collect();

    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(Style::new().reversed());

    frame.render_stateful_widget(table, area, table_state);
}

/// `TopQuery` → `Row<'static>`. Числа right-padded для визуального выравнивания
/// колонок (ratatui-Table сам не умеет per-cell alignment, поэтому делаем
/// форматированием строки).
fn top_query_to_row(q: &TopQuery) -> Row<'static> {
    Row::new([
        format!("{:>10}", q.calls),
        format!("{:>14.2}", q.total_exec_time_ms),
        format!("{:>10.2}", q.mean_exec_time_ms),
        format!("{:>10}", q.rows),
        format_query(&q.query),
    ])
}

fn format_query(q: &str) -> String {
    let one_line: String = q.split_whitespace().collect::<Vec<_>>().join(" ");
    match one_line.char_indices().nth(80) {
        Some((cutoff, _)) => format!("{}…", &one_line[..cutoff]),
        None => one_line,
    }
}
