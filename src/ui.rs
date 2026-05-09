//! TUI-слой: RAII-обёртка над `ratatui::Terminal` и render-функция кадра.
//!
//! Phase 3: render читает живые данные из `App::backends` (от collector'а через
//! `watch`-канал). Mock-данные удалены вместе с Phase 2.

use std::io::{self, Stdout};

use chrono::{DateTime, Utc};
use color_eyre::eyre::{Context, Result};
use crossterm::{
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Paragraph, Row, Table},
};

use crate::{app::App, db::Backend};

/// `ratatui::Terminal` параметризован backend'ом, а `CrosstermBackend` —
/// типом стрима, в который шлёт ANSI-байты. Зафиксировав `Stdout`,
/// говорим: «писать в реальный stdout процесса». В юнит-тестах backend
/// можно подменить на `TestBackend` — render-логика не изменится.
pub type Tui = Terminal<CrosstermBackend<Stdout>>;

/// RAII-обёртка над `ratatui::Terminal`: переводит терминал в TUI-режим
/// при создании и восстанавливает при дропе.
///
/// Drop запускается на конце scope'а — нормальный return, ?-ошибка или
/// panic-unwinding. Дополнительно ставим panic hook: он вызывается **до**
/// unwinding'а и Drop'ов, и тоже делает cleanup — иначе panic-стек ушёл бы
/// в alt-screen и пропал. Hook + Drop идемпотентны (повторный
/// `LeaveAlternateScreen`/`disable_raw_mode` безвредны).
pub struct TerminalGuard {
    terminal: Tui,
}

impl TerminalGuard {
    pub fn new() -> Result<Self> {
        enable_raw_mode().wrap_err("enable raw mode")?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen).wrap_err("enter alternate screen")?;

        Self::install_panic_hook();

        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend).wrap_err("create ratatui terminal")?;
        Ok(Self { terminal })
    }

    pub fn terminal(&mut self) -> &mut Tui {
        &mut self.terminal
    }

    fn install_panic_hook() {
        let original = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = restore_disciplines();
            original(info);
        }));
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = restore_disciplines();
    }
}

fn restore_disciplines() -> io::Result<()> {
    execute!(io::stdout(), LeaveAlternateScreen)?;
    disable_raw_mode()?;
    Ok(())
}

/// Один кадр UI: внешний `Block`, внутри — `Table` с реальными бэкендами
/// и `Paragraph`-footer с подсказками.
pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    let title = format!(" pgtop — Activity ({} backends) ", app.backends.len());
    let block = Block::bordered().title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [table_area, footer_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(inner);

    render_table(frame, table_area, app);
    render_footer(frame, footer_area);
}

fn render_table(frame: &mut Frame, area: Rect, app: &mut App) {
    let header_style = Style::new().add_modifier(Modifier::BOLD);
    let header =
        Row::new(["pid", "user", "state", "wait", "duration", "query"]).style(header_style);

    let widths = [
        Constraint::Length(7),
        Constraint::Length(14),
        Constraint::Length(20),
        Constraint::Length(15),
        Constraint::Length(10),
        Constraint::Min(0),
    ];

    // Считаем «сейчас» один раз на кадр, чтобы duration в разных строках был
    // консистентен по единой точке отсчёта.
    let now = Utc::now();
    let rows = app.backends.iter().map(|b| backend_to_row(b, now));

    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(Style::new().reversed());

    frame.render_stateful_widget(table, area, &mut app.table_state);
}

/// `Backend` → `Row<'static>` со строками-владельцами.
///
/// Каждая ячейка — `String`, поэтому `Row<'static>`: ratatui-Cell хранит
/// `Cow<'a, str>`, для owned `String` лайфтайм 'static. Никаких borrow-проблем,
/// при следующем рендере все Row уйдут в дроп — нормальная стоимость для
/// 5-50 backend'ов.
fn backend_to_row(b: &Backend, now: DateTime<Utc>) -> Row<'static> {
    Row::new([
        b.pid.to_string(),
        b.usename.clone().unwrap_or_else(em_dash),
        b.state.clone().unwrap_or_else(em_dash),
        format_wait(b),
        format_duration(b.query_start, now),
        format_query(b.query.as_deref()),
    ])
}

/// `wait_event_type:wait_event` объединяем в одно поле для компактности.
fn format_wait(b: &Backend) -> String {
    match (&b.wait_event_type, &b.wait_event) {
        (Some(t), Some(e)) => format!("{t}: {e}"),
        (Some(t), None) => t.clone(),
        (None, Some(e)) => e.clone(),
        (None, None) => em_dash(),
    }
}

/// Длительность от `query_start` до now: `H:MM:SS`. None → «—».
fn format_duration(query_start: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    let Some(start) = query_start else {
        return em_dash();
    };
    let total_secs = (now - start).num_seconds().max(0);
    let h = total_secs / 3600;
    let m = (total_secs % 3600) / 60;
    let s = total_secs % 60;
    format!("{h}:{m:02}:{s:02}")
}

/// SQL-запрос: схлопываем whitespace в один пробел и режем до 60 символов
/// (через `char_indices`, чтобы не разорвать UTF-8).
fn format_query(query: Option<&str>) -> String {
    let Some(q) = query else {
        return em_dash();
    };
    let one_line: String = q.split_whitespace().collect::<Vec<_>>().join(" ");
    match one_line.char_indices().nth(60) {
        Some((cutoff, _)) => format!("{}…", &one_line[..cutoff]),
        None => one_line,
    }
}

/// Заглушка для отсутствующих значений. Em-dash единообразно во всех колонках.
fn em_dash() -> String {
    "—".to_string()
}

fn render_footer(frame: &mut Frame, area: Rect) {
    let line = Line::from(vec![
        Span::raw(" "),
        "q".bold(),
        Span::raw(" / "),
        "Esc".bold(),
        Span::raw(" quit  ·  "),
        "↑".bold(),
        Span::raw(" "),
        "↓".bold(),
        Span::raw(" move  ·  "),
        "Enter".bold(),
        Span::raw(" details"),
    ]);

    let footer = Paragraph::new(line).style(Style::new().dim());
    frame.render_widget(footer, area);
}
