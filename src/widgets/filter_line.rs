//! Filter status line drawn between content and footer.

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

/// Render the filter line. Shows (in priority order): the filter prompt in
/// `Mode::Filter`, the active filter pattern, the last action result, or
/// nothing.
pub fn render_filter_line(frame: &mut Frame, area: Rect, app: &App) {
    let conn = app.active();
    let line = if matches!(app.mode, Mode::Filter) {
        filter_input_line(conn)
    } else if let Mode::JumpToPid(ref input) = app.mode {
        jump_input_line(input)
    } else if conn.filter.regex.is_some() {
        filter_status_line(conn)
    } else if let Some(result) = &conn.last_action_result {
        action_result_line(result, app.theme)
    } else {
        return;
    };

    frame.render_widget(Paragraph::new(line), area);
}

fn jump_input_line(input: &str) -> Line<'static> {
    Line::from(vec![
        Span::raw(" jump to pid: "),
        Span::raw(input.to_string()).bold(),
        "█".bold(),
    ])
}

fn filter_input_line(conn: &crate::app::ConnectionState) -> Line<'static> {
    let value = conn.filter.input.value();
    let invalid = !value.is_empty() && conn.filter.regex.is_none();
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

fn filter_status_line(conn: &crate::app::ConnectionState) -> Line<'static> {
    Line::from(vec![
        " filter: ".dim(),
        conn.filter.input.value().to_string().dim(),
    ])
}

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

#[allow(dead_code)]
fn _force_action_command_use(_: &ActionCommand) {}
