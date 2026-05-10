//! Filter status line — между контентом и footer'ом. 1 строка высотой,
//! рисуется (или нет) в зависимости от `Mode` и наличия активного фильтра.

use ratatui::{
    Frame,
    layout::Rect,
    style::Stylize,
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::app::{App, Mode};

/// - В `Mode::Filter`: показываем `/` + текущий ввод + маркер курсора `█`,
///   индикатор `(invalid regex)` если regex не скомпилировался.
/// - С активным фильтром в других режимах: dim-строка `filter: pattern`.
/// - Иначе пусто (1 строка зарезервирована, ничего не рендерим).
pub fn render_filter_line(frame: &mut Frame, area: Rect, app: &App) {
    let line = if matches!(app.mode, Mode::Filter) {
        let value = app.filter.input.value();
        let invalid = !value.is_empty() && app.filter.regex.is_none();
        let mut spans = vec![
            Span::raw(" /"),
            Span::raw(value.to_string()).bold(),
            "█".bold(),
        ];
        if invalid {
            spans.push(Span::raw("  "));
            spans.push("(invalid regex)".red());
        }
        Line::from(spans)
    } else if app.filter.regex.is_some() {
        Line::from(vec![
            " filter: ".dim(),
            app.filter.input.value().to_string().dim(),
        ])
    } else {
        return;
    };

    frame.render_widget(Paragraph::new(line), area);
}
