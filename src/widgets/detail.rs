//! Detail popup for the selected backend, drawn while `Mode::Detail(pid)` is
//! active.

use chrono::{DateTime, Utc};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::Stylize,
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Wrap},
};

use crate::db::Backend;

/// Render the centered detail popup for one backend.
pub fn render_detail(frame: &mut Frame, area: Rect, b: &Backend) {
    let popup = centered_rect(80, 70, area);

    frame.render_widget(Clear, popup);

    let block = Block::bordered().title(format!(" Detail · pid {} ", b.pid));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let lines = build_detail_lines(b);
    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(para, inner);
}

fn build_detail_lines(b: &Backend) -> Vec<Line<'static>> {
    vec![
        kv("user", b.usename.as_deref()),
        kv("database", b.datname.as_deref()),
        kv("application", b.application_name.as_deref()),
        kv("client", b.client_addr.as_deref()),
        kv("backend type", b.backend_type.as_deref()),
        Line::from(""),
        kv("state", b.state.as_deref()),
        Line::from(vec!["wait: ".bold(), Span::raw(format_wait(b))]),
        Line::from(""),
        kv_dt("backend started", b.backend_start),
        kv_dt("tx started", b.xact_start),
        kv_dt("query started", b.query_start),
        kv_dt("state changed", b.state_change),
        Line::from(""),
        kv("xid", b.backend_xid.as_deref()),
        kv("xmin", b.backend_xmin.as_deref()),
        Line::from(""),
        Line::from("query:".bold()),
        Line::from(b.query.clone().unwrap_or_else(em_dash)),
    ]
}

fn kv(label: &'static str, value: Option<&str>) -> Line<'static> {
    Line::from(vec![
        format!("{label}: ").bold(),
        Span::raw(value.unwrap_or("—").to_string()),
    ])
}

fn kv_dt(label: &'static str, value: Option<DateTime<Utc>>) -> Line<'static> {
    let s = value
        .map(|t| t.format("%H:%M:%S").to_string())
        .unwrap_or_else(em_dash);
    Line::from(vec![format!("{label}: ").bold(), Span::raw(s)])
}

fn format_wait(b: &Backend) -> String {
    match (&b.wait_event_type, &b.wait_event) {
        (Some(t), Some(e)) => format!("{t}: {e}"),
        (Some(t), None) => t.clone(),
        (None, Some(e)) => e.clone(),
        (None, None) => em_dash(),
    }
}

fn em_dash() -> String {
    "—".to_string()
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let [_, mid_v, _] = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .areas(area);

    let [_, mid_h, _] = Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .areas(mid_v);

    mid_h
}
