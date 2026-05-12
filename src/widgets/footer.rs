//! Footer with keymap hints. Mode- and tab-aware.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::app::{App, Mode, Tab};

/// Separator between hint groups in the footer.
const SEP: &str = "  ·  ";

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
        Mode::Explain(_) => Line::from(vec![Span::raw(" "), "Esc".bold(), Span::raw(" close")]),
        Mode::JumpToPid(_) => Line::from(vec![
            Span::raw(" "),
            "Enter".bold(),
            Span::raw(" jump  ·  "),
            "Esc".bold(),
            Span::raw(" cancel"),
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
        spans.extend([Span::raw(SEP), "Alt+N".bold(), Span::raw(" conn")]);
    }

    if tab == Tab::Activity {
        spans.extend([
            Span::raw(SEP),
            "Enter".bold(),
            Span::raw(" details  ·  "),
            "/".bold(),
            Span::raw(" filter  ·  "),
            "s".bold(),
            Span::raw("/"),
            "S".bold(),
            Span::raw(" sort  ·  "),
            "e".bold(),
            Span::raw(" explain  ·  "),
            "g".bold(),
            Span::raw(" jump"),
        ]);

        if actions_allowed {
            spans.extend([
                Span::raw(SEP),
                "c".bold().yellow(),
                Span::raw(" cancel").yellow(),
                Span::raw(SEP),
                "K".bold().red(),
                Span::raw(" terminate").red(),
            ]);
        }
    } else if tab == Tab::TopQueries {
        spans.extend([Span::raw(SEP), "x".bold(), Span::raw(" export json")]);
    } else if actions_allowed {
        spans.extend([Span::raw(SEP), "actions".bold().yellow()]);
    }

    Line::from(spans)
}
