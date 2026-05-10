//! Filter status line — между контентом и footer'ом. 1 строка высотой,
//! рисуется (или нет) в зависимости от `Mode` и наличия активного фильтра.

use ratatui::{
    Frame,
    layout::Rect,
    style::Stylize,
    text::{Line, Span},
    widgets::Paragraph,
};

use ratatui::style::Style;

use crate::actions::{ActionCommand, ActionResult};
use crate::app::{App, Mode};
use crate::theme::Theme;

/// Показывает (по приоритету):
/// 1. В `Mode::Filter`: `/`-prompt + текущий ввод + cursor + invalid-индикатор.
/// 2. Если фильтр активен (regex Some) и не Filter mode: dim-строка `filter: pattern`.
/// 3. Если есть `last_action_result`: статус последней action-команды.
/// 4. Иначе пусто.
///
/// Filter имеет приоритет над action-result'ом, потому что при наборе фильтра
/// пользователь активно набирает — статус не должен мешать.
pub fn render_filter_line(frame: &mut Frame, area: Rect, app: &App) {
    let line = if matches!(app.mode, Mode::Filter) {
        filter_input_line(app)
    } else if app.filter.regex.is_some() {
        filter_status_line(app)
    } else if let Some(result) = &app.last_action_result {
        action_result_line(result, app.theme)
    } else {
        return;
    };

    frame.render_widget(Paragraph::new(line), area);
}

fn filter_input_line(app: &App) -> Line<'static> {
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
}

fn filter_status_line(app: &App) -> Line<'static> {
    Line::from(vec![
        " filter: ".dim(),
        app.filter.input.value().to_string().dim(),
    ])
}

/// Цветовое кодирование результата (через theme):
/// - Ok(true) → `theme.success` «✓ ok».
/// - Ok(false) → `theme.warning` «no-op» — функция вернула false (нет
///   такого pid'а или нет permission'ов).
/// - Err → `theme.danger` — SQL-ошибка.
fn action_result_line(result: &ActionResult, theme: Theme) -> Line<'static> {
    let pid = result.command.pid();
    let action = result.command.label();
    let (icon, color, msg) = match &result.outcome {
        Ok(true) => ("✓", theme.success, format!(" {action} pid {pid}: sent")),
        Ok(false) => (
            "⚠",
            theme.warning,
            format!(" {action} pid {pid}: no such backend or insufficient permission"),
        ),
        Err(e) => ("✗", theme.danger, format!(" {action} pid {pid}: {e}")),
    };
    Line::from(vec![
        " ".into(),
        Span::styled(icon, Style::new().fg(color)).bold(),
        Span::styled(msg, Style::new().fg(color)),
    ])
}

// `ActionCommand` импортируется только для проверки доступности;
// форматирование делает `label()`/`pid()`. Просто чтобы не было unused warning.
#[allow(dead_code)]
fn _force_action_command_use(_: &ActionCommand) {}
