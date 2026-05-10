//! TUI orchestration: TerminalGuard и top-level render-dispatch.
//!
//! Phase 5 block A: render режет inner area на tab bar / content / filter line /
//! footer; контент диспатчится по `app.current_tab`. Tab-specific render
//! живёт в `views/`, переиспользуемые виджеты — в `widgets/`.

use std::io::{self, Stdout};

use color_eyre::eyre::{Context, Result};
use crossterm::{
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    widgets::Block,
};

use crate::{
    app::{App, Mode, Tab},
    db::TopQueriesSnapshot,
    views::{render_activity, render_locks, render_replication, render_top_queries},
    widgets::{confirm, detail, filter_line, footer, sparklines, tabs},
};

/// `ratatui::Terminal` параметризован backend'ом. `CrosstermBackend<Stdout>` —
/// «писать в реальный stdout процесса». В юнит-тестах backend меняется на
/// `TestBackend`, render-логика остаётся той же.
pub type Tui = Terminal<CrosstermBackend<Stdout>>;

/// RAII-обёртка над `ratatui::Terminal`: переводит терминал в TUI-режим
/// при создании и восстанавливает при дропе.
///
/// Drop запускается на конце scope'а (нормальный return, ?-Err, panic-unwinding).
/// Дополнительно ставим panic hook: он вызывается **до** unwinding'а и Drop'ов,
/// и тоже делает cleanup — иначе panic-стек ушёл бы в alt-screen и пропал.
/// Hook + Drop идемпотентны.
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

/// Главный entry point рендера. Структура:
/// 1. Outer `Block::bordered()` с заголовком (зависит от current_tab).
/// 2. Внутри inner-области: tab bar (1 line) + content (Min(0)) + filter
///    line (1) + footer (1).
/// 3. Content диспатчится по `app.current_tab` — Activity рисует таблицу,
///    остальные — placeholder'ы до соответствующих блоков Phase 5.
/// 4. Если `Mode::Detail(pid)` — overlay popup поверх всего.
pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    let title = title_for(app);
    let block = Block::bordered().title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [
        tabs_area,
        sparklines_area,
        content_area,
        filter_area,
        footer_area,
    ] = Layout::vertical([
        Constraint::Length(1), // tabs
        Constraint::Length(3), // header sparklines: top-border + title (1) + bars (2)
        Constraint::Min(0),    // tab content
        Constraint::Length(1), // filter line
        Constraint::Length(1), // footer hints
    ])
    .areas(inner);

    tabs::render_tab_bar(frame, tabs_area, app.current_tab);
    sparklines::render_sparklines(frame, sparklines_area, &app.stats);

    match app.current_tab {
        Tab::Activity => render_activity(frame, content_area, app),
        Tab::Locks => render_locks(frame, content_area, app),
        Tab::TopQueries => render_top_queries(frame, content_area, app),
        Tab::Replication => render_replication(frame, content_area, app),
    }

    filter_line::render_filter_line(frame, filter_area, app);
    footer::render_footer(frame, footer_area, app);

    // Modal overlays — рисуются последними поверх всего. Только один
    // активный режим за раз, поэтому match без конфликта.
    match &app.mode {
        Mode::Detail(pid) => {
            let pid = *pid;
            if let Some(b) = app.backends.iter().find(|b| b.pid == pid) {
                detail::render_detail(frame, area, b);
            }
        }
        Mode::ConfirmCancel(pid) => {
            confirm::render_confirm_cancel(frame, area, *pid);
        }
        Mode::ConfirmTerminate(pid, typed) => {
            confirm::render_confirm_terminate(frame, area, *pid, typed);
        }
        _ => {}
    }
}

/// Заголовок-рамки: на Activity показываем счётчик backend'ов (с учётом
/// фильтра); на Locks — счётчик локов с количеством waiting; на остальных
/// табах — просто имя таба.
fn title_for(app: &App) -> String {
    let tab = app.current_tab.label();
    match app.current_tab {
        Tab::Activity => {
            let visible = app.filtered.len();
            let total = app.backends.len();
            if visible == total {
                format!(" pgtop — {tab} ({total} backends) ")
            } else {
                format!(" pgtop — {tab} ({visible}/{total} backends) ")
            }
        }
        Tab::Locks => {
            let total = app.locks.len();
            let waiting = app.locks.iter().filter(|l| !l.granted).count();
            if waiting == 0 {
                format!(" pgtop — {tab} ({total} locks) ")
            } else {
                format!(" pgtop — {tab} ({waiting} waiting / {total} locks) ")
            }
        }
        Tab::TopQueries => match &app.top_queries {
            TopQueriesSnapshot::Available(queries) => {
                format!(" pgtop — {tab} (top {}) ", queries.len())
            }
            _ => format!(" pgtop — {tab} "),
        },
        Tab::Replication => {
            let count = app.replication.len();
            if count == 0 {
                format!(" pgtop — {tab} (no replicas) ")
            } else {
                format!(" pgtop — {tab} ({count}) ")
            }
        }
    }
}
