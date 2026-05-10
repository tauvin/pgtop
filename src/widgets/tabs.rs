//! Tab bar with the active tab highlighted.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::Line,
    widgets::Tabs,
};

use strum::IntoEnumIterator;

use crate::app::Tab;

pub fn render_tab_bar(frame: &mut Frame, area: Rect, current: Tab) {
    let titles: Vec<Line> = Tab::iter().map(|t| Line::from(t.label())).collect();
    let tabs = Tabs::new(titles)
        .select(current.index())
        .style(Style::new())
        .highlight_style(Style::new().add_modifier(Modifier::BOLD | Modifier::REVERSED))
        .divider("│");
    frame.render_widget(tabs, area);
}
