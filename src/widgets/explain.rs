//! EXPLAIN popup: centered, bordered, scroll-free for now.

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Clear, Paragraph, Wrap},
};

use crate::app::ExplainPopup;

pub fn render_explain(frame: &mut Frame, area: Rect, popup: &ExplainPopup) {
    let popup_area = centered(area, 70, 80);

    frame.render_widget(Clear, popup_area);

    let (title, body, body_style) = match popup {
        ExplainPopup::Loading { pid } => (
            format!(" EXPLAIN — pid {pid} "),
            Text::from(vec![Line::from(""), Line::from("  running EXPLAIN…")]),
            Style::new().add_modifier(Modifier::DIM),
        ),
        ExplainPopup::Ready { pid, plan } => {
            let lines: Vec<Line> = plan.lines().map(|l| Line::from(l.to_string())).collect();
            (
                format!(" EXPLAIN — pid {pid} "),
                Text::from(lines),
                Style::default(),
            )
        }
        ExplainPopup::Error { pid, message } => (
            format!(" EXPLAIN failed — pid {pid} "),
            Text::from(vec![
                Line::from(""),
                Line::from(Span::styled(message.clone(), Style::new().fg(Color::Red))),
            ]),
            Style::default(),
        ),
    };

    let block = Block::bordered()
        .title(title)
        .title_bottom(Line::from(" Esc to close ").right_aligned());

    let para = Paragraph::new(body)
        .block(block)
        .style(body_style)
        .wrap(Wrap { trim: false });

    frame.render_widget(para, popup_area);
}

fn centered(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(area);
    let horizontal = Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(vertical[1]);
    horizontal[1]
}
