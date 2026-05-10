//! TUI orchestration: terminal RAII guard and the top-level render dispatch.

use std::io::{self, Stdout};

use color_eyre::eyre::{Context, Result};
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    widgets::Block,
};

use chrono::Utc;

use crate::{
    app::{App, ConnectionState, ConnectionStatus, Mode, Tab},
    db::TopQueriesSnapshot,
    views::{
        render_activity, render_databases, render_locks, render_replication, render_tables,
        render_top_queries, render_waits,
    },
    widgets::{confirm, detail, explain, filter_line, footer, sparklines, tabs},
};

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

/// RAII wrapper around `ratatui::Terminal`. Switches the terminal into TUI
/// mode on construction and restores it on drop. A panic hook performs the
/// same cleanup before unwinding.
pub struct TerminalGuard {
    terminal: Tui,
}

impl TerminalGuard {
    pub fn new() -> Result<Self> {
        enable_raw_mode().wrap_err("enable raw mode")?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
            .wrap_err("enter alternate screen")?;

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
    execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen)?;
    disable_raw_mode()?;
    Ok(())
}

/// Top-level render entry point. Lays out the outer block, tab bar, header
/// sparklines, tab content, filter line, and footer; renders modal overlays
/// last.
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
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    tabs::render_tab_bar(frame, tabs_area, app.current_tab);
    sparklines::render_sparklines(frame, sparklines_area, &app.active().stats);

    match app.current_tab {
        Tab::Activity => render_activity(frame, content_area, app),
        Tab::Locks => render_locks(frame, content_area, app),
        Tab::TopQueries => render_top_queries(frame, content_area, app),
        Tab::Replication => render_replication(frame, content_area, app),
        Tab::Databases => render_databases(frame, content_area, app),
        Tab::Tables => render_tables(frame, content_area, app),
        Tab::Waits => render_waits(frame, content_area, app),
    }

    filter_line::render_filter_line(frame, filter_area, app);
    footer::render_footer(frame, footer_area, app);

    match &app.mode {
        Mode::Detail(pid) => {
            let pid = *pid;
            if let Some(b) = app.active().backends.iter().find(|b| b.pid == pid) {
                detail::render_detail(frame, area, b);
            }
        }
        Mode::ConfirmCancel(pid) => {
            confirm::render_confirm_cancel(frame, area, *pid);
        }
        Mode::ConfirmTerminate(pid, typed) => {
            confirm::render_confirm_terminate(frame, area, *pid, typed);
        }
        Mode::Explain(popup) => {
            explain::render_explain(frame, area, popup);
        }
        _ => {}
    }
}

fn title_for(app: &App) -> String {
    let prefix = build_prefix(app);
    let tab = app.current_tab.label();
    let suffix = tab_suffix(app);
    if suffix.is_empty() {
        format!(" {prefix} — {tab} ")
    } else {
        format!(" {prefix} — {tab} ({suffix}) ")
    }
}

fn build_prefix(app: &App) -> String {
    let mut prefix = String::from("pgtop");
    let conn = app.active();
    if let Some(profile) = &conn.profile_name {
        prefix.push_str(" · ");
        prefix.push_str(profile);
    }
    if conn.read_only {
        prefix.push_str(" · RO");
    }
    let total = app.connections.len();
    if total > 1 {
        prefix.push_str(&format!(" · {}/{}", app.active + 1, total));
    }
    if let ConnectionStatus::Connecting { attempt } = conn.status {
        if attempt <= 1 {
            prefix.push_str(" · connecting…");
        } else {
            prefix.push_str(&format!(" · connecting #{attempt}…"));
        }
    }
    prefix
}

fn tab_suffix(app: &App) -> String {
    let conn = app.active();
    match app.current_tab {
        Tab::Activity => {
            let visible = conn.filtered.len();
            let total = conn.backends.len();
            let slow = count_slow(conn);
            let base = if visible == total {
                format!("{total} backends")
            } else {
                format!("{visible}/{total} backends")
            };
            if slow > 0 {
                format!("{base} · ⚠ {slow} slow")
            } else {
                base
            }
        }
        Tab::Locks => {
            let total = conn.locks.len();
            let waiting = conn.locks.iter().filter(|l| !l.granted).count();
            if waiting == 0 {
                format!("{total} locks")
            } else {
                format!("{waiting} waiting / {total} locks")
            }
        }
        Tab::TopQueries => match &conn.top_queries {
            TopQueriesSnapshot::Available(queries) => format!("top {}", queries.len()),
            _ => String::new(),
        },
        Tab::Replication => {
            let count = conn.replication.len();
            if count == 0 {
                "no replicas".to_string()
            } else {
                count.to_string()
            }
        }
        Tab::Databases => {
            let count = conn.databases.len();
            if count == 0 {
                String::new()
            } else {
                format!("{count} databases")
            }
        }
        Tab::Tables => {
            let count = conn.tables.len();
            if count == 0 {
                String::new()
            } else {
                format!("top {count} tables")
            }
        }
        Tab::Waits => {
            let count: u32 = conn.waits.iter().map(|w| w.count).sum();
            if count == 0 {
                String::new()
            } else {
                format!("{count} waiting")
            }
        }
    }
}

fn count_slow(conn: &ConnectionState) -> usize {
    let now = Utc::now();
    let threshold_secs = conn.slow_query_threshold.as_secs() as i64;
    conn.backends
        .iter()
        .filter(|b| {
            !b.is_self()
                && b.state.as_deref() == Some("active")
                && b.query_start
                    .map(|s| (now - s).num_seconds() > threshold_secs)
                    .unwrap_or(false)
        })
        .count()
}
