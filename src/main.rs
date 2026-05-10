use std::env;
use std::ops::ControlFlow;
use std::path::PathBuf;

use clap::Parser;
use color_eyre::eyre::{Context, Result};
use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
};
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, filter};

mod actions;
mod app;
mod collectors;
mod config;
mod db;
mod explain;
mod messages;
mod persist;
mod theme;
mod ui;
mod views;
mod widgets;

#[cfg(test)]
mod snapshot_tests;

use actions::ActionCommand;
use app::{App, ConnectionState, ExplainPopup, Mode, Tab};
use config::Resolved;
use messages::UpdateMessage;

#[derive(Debug, Parser)]
#[command(
    name = "pgtop",
    about = "Postgres activity TUI monitor",
    long_about = "TUI monitor for PostgreSQL.\n\
                  Config: ~/.config/pgtop/config.toml (see config.example.toml in repo).\n\
                  Layering: CLI flags > DATABASE_URL env > profile > defaults.\n\
                  Multi-connection: pgtop prof1 prof2 ... — Alt+N to switch."
)]
struct Cli {
    /// Profile name(s) from config. Multiple profiles open multi-connection
    /// session, switchable via Alt+1/Alt+2/...
    profiles: Vec<String>,

    /// Override DSN. Takes precedence over env and profile.
    #[arg(long)]
    dsn: Option<String>,

    /// Allow cancel/terminate-actions on backends. Off by default.
    /// Suppressed by `--read-only` or `read_only=true` in profile.
    #[arg(long)]
    allow_actions: bool,

    /// Force read-only — disables cancel/terminate even if `--allow-actions`
    /// is set. Useful for prod-profiles where actions should NEVER fire.
    #[arg(long)]
    read_only: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();

    let config = config::load()?;

    let resolveds: Vec<Resolved> = if cli.profiles.is_empty() {
        vec![Resolved::from_layers(
            &config,
            None,
            cli.dsn.as_deref(),
            cli.allow_actions,
            cli.read_only,
        )?]
    } else {
        cli.profiles
            .iter()
            .map(|p| {
                Resolved::from_layers(
                    &config,
                    Some(p),
                    cli.dsn.as_deref(),
                    cli.allow_actions,
                    cli.read_only,
                )
            })
            .collect::<Result<Vec<_>>>()?
    };

    let _log_guard = init_audit_log()?;
    tracing::info!(
        profiles = ?resolveds.iter().filter_map(|r| r.profile_name.as_deref()).collect::<Vec<_>>(),
        connections = resolveds.len(),
        "pgtop starting"
    );

    let (update_tx, update_rx) = mpsc::unbounded_channel::<UpdateMessage>();
    let cancel = CancellationToken::new();

    let mut connections: Vec<ConnectionState> = Vec::with_capacity(resolveds.len());
    let mut action_txs: Vec<mpsc::UnboundedSender<ActionCommand>> =
        Vec::with_capacity(resolveds.len());
    let mut handles = Vec::new();

    for (idx, resolved) in resolveds.iter().enumerate() {
        let name = resolved
            .profile_name
            .clone()
            .unwrap_or_else(|| "default".to_string());

        connections.push(ConnectionState::new(
            name,
            resolved.dsn.clone(),
            resolved.read_only,
            resolved.actions_allowed,
            resolved.profile_name.clone(),
            resolved.slow_query_threshold,
        ));

        let dsn = resolved.dsn.clone();
        let intervals = &resolved.intervals;
        handles.push(tokio::spawn(collectors::run_activity_collector(
            dsn.clone(),
            update_tx.clone(),
            idx,
            cancel.clone(),
            intervals.activity,
        )));
        handles.push(tokio::spawn(collectors::run_locks_collector(
            dsn.clone(),
            update_tx.clone(),
            idx,
            cancel.clone(),
            intervals.locks,
        )));
        handles.push(tokio::spawn(collectors::run_top_queries_collector(
            dsn.clone(),
            update_tx.clone(),
            idx,
            cancel.clone(),
            intervals.top_queries,
        )));
        handles.push(tokio::spawn(collectors::run_replication_collector(
            dsn.clone(),
            update_tx.clone(),
            idx,
            cancel.clone(),
            intervals.replication,
        )));
        handles.push(tokio::spawn(collectors::run_stats_collector(
            dsn.clone(),
            update_tx.clone(),
            idx,
            cancel.clone(),
            intervals.stats,
        )));
        handles.push(tokio::spawn(collectors::run_databases_collector(
            dsn.clone(),
            update_tx.clone(),
            idx,
            cancel.clone(),
            intervals.databases,
        )));
        handles.push(tokio::spawn(collectors::run_tables_collector(
            dsn.clone(),
            update_tx.clone(),
            idx,
            cancel.clone(),
            intervals.tables,
        )));

        let (action_tx, action_rx) = mpsc::unbounded_channel::<ActionCommand>();
        handles.push(tokio::spawn(actions::run_action_executor(
            dsn,
            action_rx,
            update_tx.clone(),
            idx,
            cancel.clone(),
        )));
        action_txs.push(action_tx);
    }

    let mut app = App::new(connections);
    app.theme = resolveds[0].theme;

    if let Some(state) = persist::load() {
        state.apply(&mut app);
    }

    let mut term = ui::TerminalGuard::new()?;
    let loop_result = run_event_loop(
        term.terminal(),
        &mut app,
        update_rx,
        action_txs,
        update_tx,
        cancel.clone(),
    )
    .await;

    drop(term);
    persist::save(&persist::UiState::from_app(&app));
    cancel.cancel();

    // 2-second budget for tasks to drop after cancellation. A stuck task
    // (DNS resolution wedged inside tokio-postgres connect, etc.) shouldn't
    // keep the process alive — abort the wait and let the runtime tear them
    // down at process exit.
    let shutdown = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        futures::future::join_all(handles),
    )
    .await;
    if shutdown.is_err() {
        tracing::warn!("background tasks did not finish within 2s of cancel; forcing exit");
    }

    loop_result
}

/// Async event loop: render, await events, dispatch.
async fn run_event_loop(
    terminal: &mut ui::Tui,
    app: &mut App,
    mut update_rx: mpsc::UnboundedReceiver<UpdateMessage>,
    action_txs: Vec<mpsc::UnboundedSender<ActionCommand>>,
    update_tx_for_explain: mpsc::UnboundedSender<UpdateMessage>,
    cancel_for_explain: CancellationToken,
) -> Result<()> {
    let mut events = EventStream::new();

    loop {
        let now = chrono::Utc::now();
        terminal.draw(|frame| ui::render(frame, app, now))?;

        tokio::select! {
            maybe_event = events.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                        if key.code == KeyCode::Char('c')
                            && key.modifiers.contains(KeyModifiers::CONTROL)
                        {
                            return Ok(());
                        }

                        if key.modifiers.contains(KeyModifiers::ALT)
                            && let KeyCode::Char(c) = key.code
                            && let Some(d) = c.to_digit(10)
                        {
                            let idx = (d as usize).saturating_sub(1);
                            app.set_active(idx);
                            continue;
                        }

                        let outcome = match &app.mode {
                            Mode::Normal => handle_normal_key(
                                app,
                                key,
                                &update_tx_for_explain,
                                &cancel_for_explain,
                            ),
                            Mode::Detail(_) => handle_detail_key(app, key),
                            Mode::Explain(_) => handle_explain_key(app, key),
                            Mode::JumpToPid(_) => handle_jump_key(app, key),
                            Mode::Filter => handle_filter_key(app, key),
                            Mode::ConfirmCancel(_) => {
                                handle_confirm_cancel_key(app, key, &action_txs)
                            }
                            Mode::ConfirmTerminate(_, _) => {
                                handle_confirm_terminate_key(app, key, &action_txs)
                            }
                        };
                        if outcome.is_break() {
                            return Ok(());
                        }
                    }
                    Some(Ok(Event::Mouse(mouse))) => match mouse.kind {
                        MouseEventKind::ScrollUp => app.select_previous(),
                        MouseEventKind::ScrollDown => app.select_next(),
                        _ => {}
                    },
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(e.into()),
                    None => return Ok(()),
                }
            }

            msg = update_rx.recv() => {
                match msg {
                    None => return Ok(()),
                    Some(UpdateMessage::Activity { conn_idx, snapshot }) => {
                        if let Some(conn) = app.connection_mut(conn_idx) {
                            conn.set_backends(snapshot);
                        }
                        if conn_idx == app.active {
                            app.maybe_close_dead_modal();
                        }
                    }
                    Some(UpdateMessage::Locks { conn_idx, snapshot }) => {
                        if let Some(conn) = app.connection_mut(conn_idx) {
                            conn.set_locks(snapshot);
                        }
                    }
                    Some(UpdateMessage::TopQueries { conn_idx, snapshot }) => {
                        if let Some(conn) = app.connection_mut(conn_idx) {
                            conn.set_top_queries(snapshot);
                        }
                    }
                    Some(UpdateMessage::Replication { conn_idx, snapshot }) => {
                        if let Some(conn) = app.connection_mut(conn_idx) {
                            conn.set_replication(snapshot);
                        }
                    }
                    Some(UpdateMessage::Databases { conn_idx, snapshot }) => {
                        if let Some(conn) = app.connection_mut(conn_idx) {
                            conn.set_databases(snapshot);
                        }
                    }
                    Some(UpdateMessage::Tables { conn_idx, snapshot }) => {
                        if let Some(conn) = app.connection_mut(conn_idx) {
                            conn.set_tables(snapshot);
                        }
                    }
                    Some(UpdateMessage::Stats { conn_idx, snapshot }) => {
                        if let Some(conn) = app.connection_mut(conn_idx) {
                            conn.push_stats(snapshot);
                        }
                    }
                    Some(UpdateMessage::ActionResult { conn_idx, result }) => {
                        app.set_action_result(conn_idx, result);
                    }
                    Some(UpdateMessage::Status { conn_idx, status }) => {
                        if let Some(conn) = app.connection_mut(conn_idx) {
                            conn.status = status;
                        }
                    }
                    Some(UpdateMessage::ExplainResult { conn_idx, plan }) => {
                        // Drop result if user already switched away — the
                        // popup_cancel token has been triggered by
                        // close_modal/set_active and the spawned task is
                        // returning a "cancelled" Err anyway.
                        if conn_idx == app.active
                            && let Mode::Explain(ref state) = app.mode
                        {
                            let pid = match state {
                                ExplainPopup::Loading { pid }
                                | ExplainPopup::Ready { pid, .. }
                                | ExplainPopup::Error { pid, .. } => *pid,
                            };
                            let popup = match plan {
                                Ok(plan) => ExplainPopup::Ready { pid, plan },
                                Err(message) => ExplainPopup::Error { pid, message },
                            };
                            app.complete_explain(popup);
                        }
                    }
                }
            }

            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}

// Per-mode key dispatchers. Each returns `ControlFlow::Break(())` to
// signal that the event loop should terminate; `Continue` keeps it running.

fn handle_normal_key(
    app: &mut App,
    key: KeyEvent,
    update_tx_for_explain: &mpsc::UnboundedSender<UpdateMessage>,
    cancel_for_explain: &CancellationToken,
) -> ControlFlow<()> {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => return ControlFlow::Break(()),

        KeyCode::Char('1') => app.set_tab(Tab::Activity),
        KeyCode::Char('2') => app.set_tab(Tab::Locks),
        KeyCode::Char('3') => app.set_tab(Tab::TopQueries),
        KeyCode::Char('4') => app.set_tab(Tab::Replication),
        KeyCode::Char('5') => app.set_tab(Tab::Databases),
        KeyCode::Char('6') => app.set_tab(Tab::Tables),
        KeyCode::Char('7') => app.set_tab(Tab::Waits),
        KeyCode::Tab => app.next_tab(),

        KeyCode::Up => app.select_previous(),
        KeyCode::Down => app.select_next(),

        KeyCode::Enter if app.current_tab == Tab::Activity => app.on_enter(),
        KeyCode::Char('/') if app.current_tab == Tab::Activity => app.enter_filter_mode(),
        KeyCode::Char('s') if app.current_tab == Tab::Activity => app.cycle_sort_column(),
        KeyCode::Char('S') if app.current_tab == Tab::Activity => app.toggle_sort_direction(),
        KeyCode::Char('c') => {
            app.try_open_confirm_cancel();
        }
        KeyCode::Char('K') => {
            app.try_open_confirm_terminate();
        }
        KeyCode::Char('g') if app.current_tab == Tab::Activity => app.enter_jump_mode(),
        KeyCode::Char('e') if app.current_tab == Tab::Activity => {
            if let Some((pid, query)) = app.selected_query() {
                // Per-popup token, child of the global shutdown token:
                // closing the popup or switching connections cancels it
                // without affecting other tasks.
                let popup_cancel = cancel_for_explain.child_token();
                app.begin_explain(pid, popup_cancel.clone());
                let dsn = app.active().dsn.clone();
                let conn_idx = app.active;
                let tx = update_tx_for_explain.clone();
                tokio::spawn(explain::run_explain(dsn, query, conn_idx, tx, popup_cancel));
            }
        }
        _ => {}
    }
    ControlFlow::Continue(())
}

fn handle_detail_key(app: &mut App, key: KeyEvent) -> ControlFlow<()> {
    match key.code {
        KeyCode::Char('q') => return ControlFlow::Break(()),
        KeyCode::Esc => app.close_modal(),
        _ => {}
    }
    ControlFlow::Continue(())
}

fn handle_explain_key(app: &mut App, key: KeyEvent) -> ControlFlow<()> {
    if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
        app.close_modal();
    }
    ControlFlow::Continue(())
}

fn handle_jump_key(app: &mut App, key: KeyEvent) -> ControlFlow<()> {
    match key.code {
        KeyCode::Esc => app.close_modal(),
        KeyCode::Enter => {
            let _ = app.try_jump_to_pid();
        }
        KeyCode::Backspace => app.jump_input_pop(),
        KeyCode::Char(c) => app.jump_input_push(c),
        _ => {}
    }
    ControlFlow::Continue(())
}

fn handle_filter_key(app: &mut App, key: KeyEvent) -> ControlFlow<()> {
    match key.code {
        KeyCode::Esc => app.exit_filter_mode(false),
        KeyCode::Enter => app.exit_filter_mode(true),
        _ => app.handle_filter_input(key),
    }
    ControlFlow::Continue(())
}

fn handle_confirm_cancel_key(
    app: &mut App,
    key: KeyEvent,
    action_txs: &[mpsc::UnboundedSender<ActionCommand>],
) -> ControlFlow<()> {
    let Mode::ConfirmCancel(pid) = &app.mode else {
        return ControlFlow::Continue(());
    };
    match key.code {
        KeyCode::Enter => {
            let pid = *pid;
            let active = app.active;
            let _ = action_txs[active].send(ActionCommand::Cancel { pid });
            app.close_modal();
        }
        KeyCode::Esc => app.close_modal(),
        _ => {}
    }
    ControlFlow::Continue(())
}

fn handle_confirm_terminate_key(
    app: &mut App,
    key: KeyEvent,
    action_txs: &[mpsc::UnboundedSender<ActionCommand>],
) -> ControlFlow<()> {
    match key.code {
        KeyCode::Esc => app.close_modal(),
        KeyCode::Enter => {
            let active = app.active;
            if let Some(pid) = app.try_confirm_terminate() {
                let _ = action_txs[active].send(ActionCommand::Terminate { pid });
            }
        }
        KeyCode::Backspace => app.terminate_input_backspace(),
        KeyCode::Char(c) => app.terminate_input_push(c),
        _ => {}
    }
    ControlFlow::Continue(())
}

/// Worker-guards for the two non-blocking writers — must outlive `main`
/// so background-flushed records aren't lost.
struct LogGuards {
    _app: tracing_appender::non_blocking::WorkerGuard,
    _audit: tracing_appender::non_blocking::WorkerGuard,
}

/// Initialise two-sink tracing:
/// - Application log: `pgtop.log.YYYY-MM-DD` rotated daily, all targets
///   except `audit`. Honours `RUST_LOG`.
/// - Audit log: `pgtop-audit.log.YYYY-MM-DD` rotated daily, only events
///   with `target = "audit"` (cancel/terminate executions).
///
/// Both files are created with mode `0600` on Unix so other users on a
/// shared host can't read who was cancelled / terminated.
fn init_audit_log() -> Result<LogGuards> {
    let log_dir = resolve_log_dir();
    std::fs::create_dir_all(&log_dir).wrap_err("create pgtop log dir")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&log_dir, std::fs::Permissions::from_mode(0o700));
    }

    let app_appender = tracing_appender::rolling::daily(&log_dir, "pgtop.log");
    let (app_writer, app_guard) = tracing_appender::non_blocking(app_appender);

    let audit_appender = tracing_appender::rolling::daily(&log_dir, "pgtop-audit.log");
    let (audit_writer, audit_guard) = tracing_appender::non_blocking(audit_appender);

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let app_layer = tracing_subscriber::fmt::layer()
        .with_writer(app_writer)
        .with_ansi(false)
        // Drop audit events from the app log to avoid duplication.
        .with_filter(filter::filter_fn(|m| m.target() != "audit"));

    let audit_layer = tracing_subscriber::fmt::layer()
        .with_writer(audit_writer)
        .with_ansi(false)
        // Audit must always record regardless of RUST_LOG: a user who
        // bumps RUST_LOG=warn to silence noise should not also lose
        // audit trail of their own cancel/terminate actions.
        .with_filter(filter::Targets::new().with_target("audit", tracing::Level::INFO));

    tracing_subscriber::registry()
        .with(env_filter)
        .with(app_layer)
        .with(audit_layer)
        .init();

    #[cfg(unix)]
    restrict_log_files(&log_dir);

    Ok(LogGuards {
        _app: app_guard,
        _audit: audit_guard,
    })
}

/// Tighten permissions to 0600 on every existing pgtop log file. Called
/// after the appenders create today's files. Idempotent — applies on
/// every start.
#[cfg(unix)]
fn restrict_log_files(log_dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(entries) = std::fs::read_dir(log_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let s = name.to_string_lossy();
        if s.starts_with("pgtop.log") || s.starts_with("pgtop-audit.log") {
            let _ = std::fs::set_permissions(entry.path(), std::fs::Permissions::from_mode(0o600));
        }
    }
}

/// Resolve the directory used for the pgtop log file. Honours
/// `PGTOP_LOG_DIR`, then XDG state dir, then platform fallbacks.
fn resolve_log_dir() -> PathBuf {
    if let Ok(custom) = env::var("PGTOP_LOG_DIR") {
        return PathBuf::from(custom);
    }

    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .or_else(|| dirs::home_dir().map(|h| h.join(".local").join("state")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("pgtop")
}
