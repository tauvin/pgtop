//! Footer с подсказками хоткеев. Mode-aware и tab-aware: в Activity показывает
//! полный набор хоткеев, на других табах — упрощённый.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::app::{App, Mode, Tab};

pub fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let line = match &app.mode {
        Mode::Normal => normal_hints(app.current_tab),
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
    };

    let footer = Paragraph::new(line).style(Style::new().dim());
    frame.render_widget(footer, area);
}

/// Хинты для Normal mode зависят от current_tab. ↑↓-нав работает везде
/// (select_previous/next сами no-op для табов без list); Activity получает
/// расширенный набор (Enter/filter/sort).
fn normal_hints(tab: Tab) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = vec![
        Span::raw(" "),
        "q".bold(),
        Span::raw(" quit  ·  "),
        "1234".bold(),
        Span::raw(" tabs  ·  "),
        "↑↓".bold(),
        Span::raw(" move"),
    ];

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
    }

    Line::from(spans)
}
