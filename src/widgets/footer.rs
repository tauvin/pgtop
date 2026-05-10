//! Footer with keymap hints. Mode- and tab-aware.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::app::{App, Mode, Tab};

pub fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let multi_conn = app.connections.len() > 1;
    let line = match &app.mode {
        Mode::Normal => normal_hints(app.current_tab, app.active().actions_allowed, multi_conn),
        Mode::Detail(_) => Line::from(vec![
            Span::raw(" "),
            "Esc".bold(),
            Span::raw(" close  ·  "),
            "q".bold(),
            Span::raw(" quit"),
        ]),
        Mode::Filter => Line::from(vec![
            Span::raw(" "),
            "Enter".bold(),
            Span::raw(" apply  ·  "),
            "Esc".bold(),
            Span::raw(" cancel"),
        ]),
        Mode::ConfirmCancel(_) => Line::from(vec![
            Span::raw(" "),
            "Enter".bold(),
            Span::raw(" confirm  ·  "),
            "Esc".bold(),
            Span::raw(" abort"),
        ]),
        Mode::ConfirmTerminate(_, _) => Line::from(vec![
            Span::raw(" type "),
            "yes".bold(),
            Span::raw(" + "),
            "Enter".bold(),
            Span::raw(" to confirm  ·  "),
            "Esc".bold(),
            Span::raw(" abort"),
        ]),
    };

    let footer = Paragraph::new(line).style(Style::new().dim());
    frame.render_widget(footer, area);
}

fn normal_hints(tab: Tab, actions_allowed: bool, multi_conn: bool) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = vec![
        Span::raw(" "),
        "q".bold(),
        Span::raw(" quit  ·  "),
        "1234567".bold(),
        Span::raw(" tabs  ·  "),
        "↑↓".bold(),
        Span::raw(" move"),
    ];

    if multi_conn {
        spans.extend([Span::raw("  ·  "), "Alt+N".bold(), Span::raw(" conn")]);
    }

    if tab == Tab::Activity {
        spans.extend([
            Span::raw("  ·  "),
            "Enter".bold(),
            Span::raw(" details  ·  "),
            "/".bold(),
            Span::raw(" filter  ·  "),
            "s".bold(),
            Span::raw("/"),
            "S".bold(),
            Span::raw(" sort"),
        ]);

        if actions_allowed {
            spans.extend([
                Span::raw("  ·  "),
                "c".bold().yellow(),
                Span::raw(" cancel").yellow(),
                Span::raw("  ·  "),
                "K".bold().red(),
                Span::raw(" terminate").red(),
            ]);
        }
    } else if actions_allowed {
        spans.extend([Span::raw("  ·  "), "actions".bold().yellow()]);
    }

    Line::from(spans)
}
