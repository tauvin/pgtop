//! Activity tab view: таблица backend'ов с цветовой подсветкой по
//! state/duration и индикатором сортировки в header'е.

use chrono::{DateTime, Utc};
use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::{Color, Modifier, Style},
    widgets::{Row, Table},
};

use crate::{
    app::{App, Sort, SortBy},
    db::Backend,
};

/// Контент Activity таба: stateful Table с TableState из App.
/// `now` зафиксирован один раз на кадр для консистентности duration во всех
/// строках и для не-нарушения Ord в потенциально вложенных сортировках.
pub fn render_activity(frame: &mut Frame, area: Rect, app: &mut App) {
    let header = build_header_row(app.sort);

    let widths = [
        Constraint::Length(7),
        Constraint::Length(14),
        Constraint::Length(20),
        Constraint::Length(15),
        Constraint::Length(10),
        Constraint::Min(0),
    ];

    let now = Utc::now();
    // Собираем в Vec явно: иначе immutable borrow от `app.visible_backends()`
    // тянется до конца Table::new(...) и мешает mutable borrow на
    // `&mut app.table_state` ниже.
    let rows: Vec<Row<'static>> = app
        .visible_backends()
        .map(|b| backend_to_row(b, now))
        .collect();

    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(Style::new().reversed());

    frame.render_stateful_widget(table, area, &mut app.table_state);
}

/// Header с индикатором текущей сортировки (`▲`/`▼` после имени активной колонки).
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

fn backend_to_row(b: &Backend, now: DateTime<Utc>) -> Row<'static> {
    Row::new([
        b.pid.to_string(),
        b.usename.clone().unwrap_or_else(em_dash),
        b.state.clone().unwrap_or_else(em_dash),
        format_wait(b),
        format_duration(b.query_start, now),
        format_query(b.query.as_deref()),
    ])
    .style(row_style(b, now))
}

/// Стиль строки исходя из state и duration активного запроса.
/// Приоритет: красный > жёлтый > зелёный > default.
/// - Красный: active-запрос дольше 10с (визуальный сигнал «долгий»).
/// - Жёлтый: idle in transaction (потенциально удерживает локи / vacuum).
/// - Зелёный: обычный active (≤10с).
/// - Default: idle-сессии, fastpath function call и т.п.
fn row_style(b: &Backend, now: DateTime<Utc>) -> Style {
    const LONG_QUERY_THRESHOLD_SECS: i64 = 10;

    let state = b.state.as_deref();

    if state == Some("active") {
        let long = b
            .query_start
            .map(|s| (now - s).num_seconds() > LONG_QUERY_THRESHOLD_SECS)
            .unwrap_or(false);
        return if long {
            Style::new().fg(Color::Red)
        } else {
            Style::new().fg(Color::Green)
        };
    }

    if state.is_some_and(|s| s.starts_with("idle in transaction")) {
        return Style::new().fg(Color::Yellow);
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
