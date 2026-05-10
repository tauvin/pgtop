use std::env;
use std::path::PathBuf;

use clap::Parser;
use color_eyre::eyre::{Context, Result};
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

mod actions;
mod app;
mod collectors;
mod config;
mod db;
mod messages;
mod theme;
mod ui;
mod views;
mod widgets;

use actions::ActionCommand;
use app::{App, ConnectionState, Mode, Tab};
use config::Resolved;
use messages::UpdateMessage;

/// CLI-аргументы. Phase 8 Block B: `profiles: Vec<String>` для multi-conn —
/// `pgtop prod staging local` открывает 3 подключения, переключение
/// `Alt+1`/`Alt+2`/`Alt+3`.
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

    // Phase 7: загрузить TOML-конфиг.
    let config = config::load()?;

    // Phase 8 Block B: resolve по N профилям (или single default если
    // `cli.profiles` пуст). Каждый получает свои read_only/actions_allowed
    // и DSN с layered'ом CLI > env > profile > defaults.
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

    // Phase 8 Block B: shared mpsc fan-in. Все collector'ы и executor'ы
    // публикуют через один update_tx; main имеет один update_rx. UpdateMessage
    // несёт `conn_idx` для адресации правильного ConnectionState.
    let (update_tx, update_rx) = mpsc::unbounded_channel::<UpdateMessage>();
    let cancel = CancellationToken::new();

    // Per-connection: свой набор клиентов, action_tx, 6 spawn'нутых задач.
    let mut connections: Vec<ConnectionState> = Vec::with_capacity(resolveds.len());
    let mut action_txs: Vec<mpsc::UnboundedSender<ActionCommand>> =
        Vec::with_capacity(resolveds.len());
    let mut handles = Vec::new();

    for (idx, resolved) in resolveds.iter().enumerate() {
        // Имя для UI: profile_name либо "default" для безпрофильного запуска.
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
        ));

        // Phase 8 Block C: каждый collector / executor сам подключается с
        // backoff'ом и реконнектится при разрыве. main лишь раздаёт DSN —
        // не блокируется на стартовом подключении (DB может быть down при
        // запуске, UI всё равно поднимется и покажет "connecting...").
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

        // Per-conn action channel: команды от main → executor.
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

    // Дропаем оригинал update_tx — у нас только клоны в spawn'нутых задачах.
    // Когда все задачи завершатся (cancel'ом), все клоны дропнутся → receiver
    // увидит None. Без этого drop'а receiver не закроется даже после shutdown'а.
    drop(update_tx);

    let mut app = App::new(connections);
    app.theme = resolveds[0].theme;
    let mut term = ui::TerminalGuard::new()?;
    let loop_result = run_event_loop(term.terminal(), &mut app, update_rx, action_txs).await;

    drop(term);
    cancel.cancel();

    // Ждём все handle'ы — может быть много (6N). futures::future::join_all для
    // динамической Vec'и handle'ов; `let _ =` глушит JoinError'ы.
    let _ = futures::future::join_all(handles).await;

    loop_result
}

/// Async event loop: render → ждём первое событие → реагируем → повторяем.
///
/// Phase 8 Block B: каналы упростились — один `update_rx` ловит сообщения
/// от ВСЕХ collector'ов и executor'ов всех соединений. `action_txs` — Vec
/// per-conn, индексируется `app.active`.
async fn run_event_loop(
    terminal: &mut ui::Tui,
    app: &mut App,
    mut update_rx: mpsc::UnboundedReceiver<UpdateMessage>,
    action_txs: Vec<mpsc::UnboundedSender<ActionCommand>>,
) -> Result<()> {
    let mut events = EventStream::new();

    loop {
        terminal.draw(|frame| ui::render(frame, app))?;

        tokio::select! {
            // Гард `kind == Press` отсекает дубликаты на терминалах
            // с kitty keyboard protocol.
            maybe_event = events.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                        // Универсальный Ctrl+C перед mode-dispatch'ем.
                        if key.code == KeyCode::Char('c')
                            && key.modifiers.contains(KeyModifiers::CONTROL)
                        {
                            return Ok(());
                        }

                        // Phase 8 Block B: универсальный Alt+digit — переключение
                        // активного соединения. Идёт ДО mode-dispatch'а, чтобы
                        // работало даже из Detail/Filter/Confirm — переключение
                        // соединения сбрасывает Mode в Normal (см. set_active).
                        if key.modifiers.contains(KeyModifiers::ALT)
                            && let KeyCode::Char(c) = key.code
                            && let Some(d) = c.to_digit(10)
                        {
                            let idx = (d as usize).saturating_sub(1);
                            app.set_active(idx);
                            continue;
                        }

                        // Mode-based dispatch: каждый режим имеет свой keymap.
                        // `q` универсально выходит (кроме Filter, где `q` —
                        // обычная буква). `Esc` контекстный.
                        match &app.mode {
                            Mode::Normal => match key.code {
                                // Универсальный quit (любой таб).
                                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),

                                // Tab switching (любой таб).
                                KeyCode::Char('1') => app.set_tab(Tab::Activity),
                                KeyCode::Char('2') => app.set_tab(Tab::Locks),
                                KeyCode::Char('3') => app.set_tab(Tab::TopQueries),
                                KeyCode::Char('4') => app.set_tab(Tab::Replication),
                                KeyCode::Tab => app.next_tab(),

                                // ↑↓ работают на любом табе с навигацией: select_previous/next
                                // сами диспатчат по current_tab (no-op на табах без list).
                                KeyCode::Up => app.select_previous(),
                                KeyCode::Down => app.select_next(),

                                // Enter (Detail view) пока только в Activity.
                                KeyCode::Enter if app.current_tab == Tab::Activity => {
                                    app.on_enter()
                                }
                                KeyCode::Char('/') if app.current_tab == Tab::Activity => {
                                    app.enter_filter_mode()
                                }
                                KeyCode::Char('s') if app.current_tab == Tab::Activity => {
                                    app.cycle_sort_column()
                                }
                                KeyCode::Char('S') if app.current_tab == Tab::Activity => {
                                    app.toggle_sort_direction()
                                }
                                // `c` (Activity, --allow-actions, не self):
                                // открыть confirm-cancel модалку.
                                KeyCode::Char('c') => {
                                    app.try_open_confirm_cancel();
                                }
                                // `K` (Shift+k) — terminate с type-yes-confirm.
                                // Crossterm на большинстве терминалов
                                // прислывает заглавную букву как Char('K')
                                // (с modifier SHIFT или без — зависит от
                                // терминала). Match только на код символа.
                                KeyCode::Char('K') => {
                                    app.try_open_confirm_terminate();
                                }
                                _ => {}
                            },
                            Mode::Detail(_) => match key.code {
                                KeyCode::Char('q') => return Ok(()),
                                KeyCode::Esc => app.close_modal(),
                                _ => {}
                            },
                            Mode::Filter => match key.code {
                                KeyCode::Esc => app.exit_filter_mode(false),
                                KeyCode::Enter => app.exit_filter_mode(true),
                                // Всё остальное (буквы, цифры, backspace,
                                // стрелки курсора, Ctrl+U, Home/End...)
                                // forward'им в tui-input.
                                _ => app.handle_filter_input(key),
                            },
                            Mode::ConfirmCancel(pid) => match key.code {
                                KeyCode::Enter => {
                                    let pid = *pid;
                                    let active = app.active;
                                    // Команда уходит executor'у активного
                                    // соединения; результат прилетит через
                                    // shared update_rx с conn_idx.
                                    let _ = action_txs[active]
                                        .send(ActionCommand::Cancel { pid });
                                    app.close_modal();
                                }
                                KeyCode::Esc => app.close_modal(),
                                _ => {}
                            },
                            Mode::ConfirmTerminate(_, _) => match key.code {
                                KeyCode::Esc => app.close_modal(),
                                KeyCode::Enter => {
                                    let active = app.active;
                                    if let Some(pid) = app.try_confirm_terminate() {
                                        let _ = action_txs[active]
                                            .send(ActionCommand::Terminate { pid });
                                    }
                                }
                                KeyCode::Backspace => app.terminate_input_backspace(),
                                KeyCode::Char(c) => app.terminate_input_push(c),
                                _ => {}
                            },
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(e.into()),
                    None => return Ok(()),
                }
            }

            // Phase 8 Block B: единая ветка mpsc fan-in. UpdateMessage
            // адресует ConnectionState через `conn_idx`. Modal-cleanup
            // (Detail/Confirm на исчезнувший pid) делается только если
            // обновление пришло на активный коннект.
            msg = update_rx.recv() => {
                match msg {
                    None => return Ok(()),  // все senders дропнуты
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
                    Some(UpdateMessage::Stats { conn_idx, snapshot }) => {
                        if let Some(conn) = app.connection_mut(conn_idx) {
                            conn.push_stats(snapshot);
                        }
                    }
                    Some(UpdateMessage::ActionResult { conn_idx, result }) => {
                        // last_action_result глобальный (один на App). Показываем
                        // только результаты от активного соединения, чтобы не
                        // путать «прислала команду на prod, ушёл на staging,
                        // увидел результат с prod».
                        if conn_idx == app.active {
                            app.set_action_result(result);
                        }
                    }
                    Some(UpdateMessage::Status { conn_idx, status }) => {
                        if let Some(conn) = app.connection_mut(conn_idx) {
                            conn.status = status;
                        }
                    }
                }
            }

            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}

/// Tracing → файл в XDG-state-директории, не stdout/stderr (TUI владеет
/// терминалом). Returns `WorkerGuard` — non-blocking writer запускает фоновый
/// поток-writer, guard управляет его жизнью. Дропать guard слишком рано =
/// терять последние записи (background-writer не успеет flush'нуть).
/// Поэтому держим до конца main.
///
/// Phase 7: переезд с `~/.pgtop/pgtop.log` на XDG state dir
/// (`~/.local/state/pgtop/pgtop.log` на Linux). Override через `PGTOP_LOG_DIR`
/// для testing/CI/контейнеров где home filesystem read-only.
///
/// Audit-инфа про cancel/terminate-actions летит сюда через
/// `tracing::info!(target: "audit", ...)`. Фильтр RUST_LOG позволяет
/// раздельно настроить уровни для audit и других target'ов
/// (`RUST_LOG=audit=info`).
fn init_audit_log() -> Result<tracing_appender::non_blocking::WorkerGuard> {
    let log_dir = resolve_log_dir();
    std::fs::create_dir_all(&log_dir).wrap_err("create pgtop log dir")?;

    // `never` — не ротируем: для TUI-сессий длиной в часы-дни overkill.
    // `daily` пригодится если станет актуально (long-running mode).
    let file_appender = tracing_appender::rolling::never(&log_dir, "pgtop.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        // Без ANSI-кодов в файле — иначе grep будет ловить escape-символы.
        .with_ansi(false)
        .init();

    Ok(guard)
}

/// XDG-resolved путь для log-файла:
/// - **Linux**: `$XDG_STATE_HOME/pgtop` или `$HOME/.local/state/pgtop`.
/// - **macOS**: `state_dir` отсутствует у Apple → fallback на
///   `data_local_dir` (`~/Library/Application Support/pgtop`).
/// - **Windows**: то же — `state_dir` нет, идём в
///   `dirs::data_local_dir()` (`%LOCALAPPDATA%\pgtop`).
/// - **Override**: `PGTOP_LOG_DIR` env — для CI/тестов/read-only-home сценариев.
/// - **Last resort**: `./pgtop` относительно cwd.
fn resolve_log_dir() -> PathBuf {
    // Override env имеет наивысший приоритет.
    if let Ok(custom) = env::var("PGTOP_LOG_DIR") {
        return PathBuf::from(custom);
    }

    // Стандартный chain: XDG state → XDG data_local → fallback на $HOME/.local/state.
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .or_else(|| dirs::home_dir().map(|h| h.join(".local").join("state")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("pgtop")
}
