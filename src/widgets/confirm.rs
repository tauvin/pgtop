//! Confirmation popups for destructive actions (cancel and terminate).

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Wrap},
};

/// Render the cancel confirmation popup.
pub fn render_confirm_cancel(frame: &mut Frame, area: Rect, pid: i32) {
    let popup = centered_rect(50, 30, area);
    frame.render_widget(Clear, popup);

    let block = Block::bordered()
        .title(" Confirm cancel ")
        .border_style(Style::new().fg(Color::Yellow));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let text = vec![
        Line::from(""),
        Line::from(vec![
            Span::raw("  Send "),
            "pg_cancel_backend".bold(),
            Span::raw(" to "),
            "pid ".bold(),
            format!("{pid}").bold(),
            Span::raw("?"),
        ]),
        Line::from(""),
        Line::from("  This will interrupt the current query but"),
        Line::from("  keep the connection alive."),
        Line::from(""),
        Line::from(vec![
            "  Enter".bold(),
            Span::raw(" confirm  ·  "),
            "Esc".bold(),
            Span::raw(" abort"),
        ]),
    ];

    let para = Paragraph::new(text).wrap(Wrap { trim: false });
    frame.render_widget(para, inner);
}

/// Render the terminate confirmation popup. Requires the user to type
/// `"yes"` before Enter sends the command.
pub fn render_confirm_terminate(frame: &mut Frame, area: Rect, pid: i32, typed: &str) {
    let popup = centered_rect(54, 40, area);
    frame.render_widget(Clear, popup);

    let block = Block::bordered()
        .title(" Confirm terminate ")
        .border_style(Style::new().fg(Color::Red));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let valid = typed == "yes";
    let prompt_color = if valid { Color::Green } else { Color::Yellow };

    let text = vec![
        Line::from(""),
        Line::from(vec![
            Span::raw("  Send "),
            "pg_terminate_backend".bold().red(),
            Span::raw(" to "),
            "pid ".bold(),
            format!("{pid}").bold(),
            Span::raw("?"),
        ]),
        Line::from(""),
        Line::from(vec![
            "  This is destructive".red().add_modifier(Modifier::BOLD),
            ": it will kill the".red(),
        ]),
        Line::from("  entire session, not just the current query.".red()),
        Line::from(""),
        Line::from(vec!["  Type ".into(), "yes".bold(), " to confirm:".into()]),
        Line::from(vec![
            "    > ".into(),
            Span::raw(typed.to_string()).style(Style::new().fg(prompt_color)),
            Span::raw("█").style(Style::new().fg(prompt_color)),
        ]),
        Line::from(""),
        Line::from(vec![
            "  Enter".bold(),
            Span::raw(" send  ·  "),
            "Esc".bold(),
            Span::raw(" abort"),
        ]),
    ];

    let para = Paragraph::new(text).wrap(Wrap { trim: false });
    frame.render_widget(para, inner);
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
