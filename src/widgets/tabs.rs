//! Tab bar — ряд табов с подсветкой активного. Использует встроенный
//! `ratatui::widgets::Tabs` widget.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::Line,
    widgets::Tabs,
};

use crate::app::Tab;

pub fn render_tab_bar(frame: &mut Frame, area: Rect, current: Tab) {
    // `Tabs::new` принимает `Vec<Line>` (или другие IntoIterator-Item: Into<Line>);
    // подсветка активного — через `select(index)` + `highlight_style`.
    let titles: Vec<Line> = Tab::all().iter().map(|t| Line::from(t.label())).collect();
    let tabs = Tabs::new(titles)
        .select(current.index())
        .style(Style::new())
        .highlight_style(Style::new().add_modifier(Modifier::BOLD | Modifier::REVERSED))
        .divider("│");
    frame.render_widget(tabs, area);
}
