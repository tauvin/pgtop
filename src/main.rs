use std::env;
use std::path::PathBuf;

use clap::Parser;
use color_eyre::eyre::{Context, Result};
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

mod actions;
mod app;
mod collectors;
mod config;
mod db;
mod theme;
mod ui;
mod views;
mod widgets;

use actions::{ActionCommand, ActionResult};
use app::{App, ConnectionState, Mode, Tab};
use db::{Backend, Lock, Replica, Stats, TopQueriesSnapshot};

/// CLI-аргументы. Phase 7: добавлены `profile` (positional), `--dsn`,
/// `--read-only`. Resolved-логика layered'ов в `config::Resolved::from_layers`.
#[derive(Debug, Parser)]
#[command(
    name = "pgtop",
    about = "Postgres activity TUI monitor",
    long_about = "TUI monitor for PostgreSQL.\n\
                  Config: ~/.config/pgtop/config.toml (see config.example.toml in repo).\n\
                  Layering: CLI flags > DATABASE_URL env > profile > defaults."
)]
struct Cli {
    /// Profile name from config. Falls back to default_profile.
    profile: Option<String>,

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

    // Phase 7: загрузить TOML-конфиг и свести все layer'ы (defaults → file
    // → DATABASE_URL env → CLI) в финальный Resolved.
    let config = config::load()?;
    let resolved = config::Resolved::from_layers(
        &config,
        cli.profile.as_deref(),
        cli.dsn.as_deref(),
        cli.allow_actions,
        cli.read_only,
    )?;

    // Tracing → файл, не stdout/stderr (TUI владеет терминалом). Guard держим
    // в main до конца — при дропе non-blocking writer flush'нет буфер. Без
    // удержания guard'а строки могут потеряться, если процесс резко выйдет.
    let _log_guard = init_audit_log()?;
    tracing::info!(
        profile = ?resolved.profile_name,
        actions_allowed = resolved.actions_allowed,
        read_only = resolved.read_only,
        "pgtop starting"
    );

    let dsn = resolved.dsn.clone();
    // Каждый collector получает свой connection через отдельный `db::connect`.
    // tokio_postgres::Client не impl Clone (хотя внутри Arc'нут — public API
    // не экспонирует), и совместное использование через `Arc<Client>` тоже
    // сериализовало бы запросы через единственный driver. Параллельные
    // соединения дёшевы (Postgres легко держит десятки) и дают true-параллелизм.
    let client_activity = db::connect(&dsn).await?;
    let client_locks = db::connect(&dsn).await?;
    let client_top_queries = db::connect(&dsn).await?;
    let client_replication = db::connect(&dsn).await?;
    let client_stats = db::connect(&dsn).await?;
    // Action executor — отдельное соединение, не конкурирует с collector'ами.
    // Запросы pg_cancel_backend/pg_terminate_backend сами по себе мгновенные,
    // но если бы шли через общий driver, могли бы блочиться за collector'ами.
    let client_actions = db::connect(&dsn).await?;

    // watch::channel(initial) — latest-wins канал. Sender хранит ровно одно
    // значение, при `send` оно заменяется; Receiver-ы видят новую версию через
    // `.changed().await`. Идеально для мониторинга: UI'у нужен только свежий
    // снапшот, история не интересует.
    //
    // Initial value — пустой Vec: до первого тика collector'а UI рисует пустую
    // таблицу. Первый tick происходит сразу (interval так устроен), задержка
    // до первого реального снапшота = одно сетевое RTT к БД.
    let (activity_tx, activity_rx) = watch::channel::<Vec<Backend>>(Vec::new());
    let (locks_tx, locks_rx) = watch::channel::<Vec<Lock>>(Vec::new());
    let (top_queries_tx, top_queries_rx) =
        watch::channel::<TopQueriesSnapshot>(TopQueriesSnapshot::Loading);
    let (replication_tx, replication_rx) = watch::channel::<Vec<Replica>>(Vec::new());
    // Stats — initial value: TPS=0, conns=0, cache_hit=100% (best guess).
    let (stats_tx, stats_rx) = watch::channel::<Stats>(Stats {
        tps: 0.0,
        active_connections: 0,
        cache_hit_pct: 100.0,
    });

    // Actions: mpsc для команд (multiple producers — main task посылает по
    // мере хоткеев), watch для результата (latest-wins, UI читает последний).
    let (action_tx, action_rx) = mpsc::unbounded_channel::<ActionCommand>();
    let (action_result_tx, action_result_rx) = watch::channel::<Option<ActionResult>>(None);

    // CancellationToken — shared cancel-флаг с нотификацией. clone() шарит
    // underlying state (внутри Arc<...>); cancel с любого клона видят все.
    // main держит оригинал, collector'ы — клоны; на shutdown main зовёт cancel,
    // оба collector'а видят на ближайшей `.cancelled()` await-точке.
    let cancel = CancellationToken::new();

    let activity_handle = tokio::spawn(collectors::run_activity_collector(
        client_activity,
        activity_tx,
        cancel.clone(),
        resolved.intervals.activity,
    ));
    let locks_handle = tokio::spawn(collectors::run_locks_collector(
        client_locks,
        locks_tx,
        cancel.clone(),
        resolved.intervals.locks,
    ));
    let top_queries_handle = tokio::spawn(collectors::run_top_queries_collector(
        client_top_queries,
        top_queries_tx,
        cancel.clone(),
        resolved.intervals.top_queries,
    ));
    let replication_handle = tokio::spawn(collectors::run_replication_collector(
        client_replication,
        replication_tx,
        cancel.clone(),
        resolved.intervals.replication,
    ));
    let stats_handle = tokio::spawn(collectors::run_stats_collector(
        client_stats,
        stats_tx,
        cancel.clone(),
        resolved.intervals.stats,
    ));
    let action_handle = tokio::spawn(actions::run_action_executor(
        client_actions,
        action_rx,
        action_result_tx,
        cancel.clone(),
    ));

    // Phase 8 block A: одно соединение пока, но архитектурно App ждёт Vec.
    // Block B расширит до multi-profile через `pgtop [PROFILE...]` CLI.
    let conn = ConnectionState::new(
        resolved
            .profile_name
            .clone()
            .unwrap_or_else(|| "default".to_string()),
        resolved.dsn.clone(),
        resolved.read_only,
        resolved.actions_allowed,
        resolved.profile_name.clone(),
    );
    let mut app = App::new(vec![conn]);
    app.theme = resolved.theme;
    let mut term = ui::TerminalGuard::new()?;
    let loop_result = run_event_loop(
        term.terminal(),
        &mut app,
        activity_rx,
        locks_rx,
        top_queries_rx,
        replication_rx,
        stats_rx,
        action_tx,
        action_result_rx,
    )
    .await;

    // Восстанавливаем терминал ДО shutdown'а collector'ов: иначе пользователь
    // видит замороженный кадр пока мы ждём завершение фоновых тасок (~до 1с).
    drop(term);

    // Сигнал всем collector'ам. Cancel идемпотентен и идёт через клон токена.
    cancel.cancel();

    // Ждём все handle'ы. `tokio::join!` завершается, когда **все** future'ы
    // готовы. `let _ =` глушит JoinError'ы от потенциальной паники в таске.
    let _ = tokio::join!(
        activity_handle,
        locks_handle,
        top_queries_handle,
        replication_handle,
        stats_handle,
        action_handle,
    );

    loop_result
}

/// Async event loop: render → ждём первое событие → реагируем → повторяем.
///
/// Три ветки `select!`:
/// - `events.next()` — клавиатура, ресайз, мышь.
/// - `data_rx.changed()` — collector прислал свежий snapshot.
/// - `signal::ctrl_c()` — SIGINT.
///
/// Все cancel-safe (см. tokio-доку для каждого). Render вверху loop'а
/// перерисовывает кадр по событию любого типа: на любую клавишу или новый
/// snapshot UI обновляется сразу. Никаких 60fps-tick'ов: render строго по
/// причине, ratatui-diff делает no-op-кадры дешёвыми.
#[allow(clippy::too_many_arguments)] // 7 channels + terminal + app — refactor in Phase 7
async fn run_event_loop(
    terminal: &mut ui::Tui,
    app: &mut App,
    mut activity_rx: watch::Receiver<Vec<Backend>>,
    mut locks_rx: watch::Receiver<Vec<Lock>>,
    mut top_queries_rx: watch::Receiver<TopQueriesSnapshot>,
    mut replication_rx: watch::Receiver<Vec<Replica>>,
    mut stats_rx: watch::Receiver<Stats>,
    action_tx: mpsc::UnboundedSender<ActionCommand>,
    mut action_result_rx: watch::Receiver<Option<ActionResult>>,
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
                        // Универсальный Ctrl+C перед mode-dispatch'ем: в raw
                        // mode терминальный драйвер обычно НЕ транслирует
                        // Ctrl+C в SIGINT (флаг ISIG снят). Поэтому
                        // `tokio::signal::ctrl_c()` ниже сработает только
                        // от внешнего `kill -INT`, а от клавиатуры —
                        // приходит как `Char('c') + CONTROL`. Ловим явно.
                        if key.code == KeyCode::Char('c')
                            && key.modifiers.contains(KeyModifiers::CONTROL)
                        {
                            return Ok(());
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
                                    // try_send / non-blocking: event-loop не
                                    // ждёт executor'а, результат прилетит
                                    // через action_result_rx.changed().
                                    let _ = action_tx
                                        .send(ActionCommand::Cancel { pid });
                                    app.close_modal();
                                }
                                KeyCode::Esc => app.close_modal(),
                                _ => {}
                            },
                            Mode::ConfirmTerminate(_, _) => match key.code {
                                // Esc — abort всегда. Enter — отправить только
                                // если text == "yes" (проверяет
                                // try_confirm_terminate). Иначе модалка
                                // остаётся открытой — anti-fool design.
                                KeyCode::Esc => app.close_modal(),
                                KeyCode::Enter => {
                                    if let Some(pid) = app.try_confirm_terminate() {
                                        let _ = action_tx
                                            .send(ActionCommand::Terminate { pid });
                                    }
                                }
                                KeyCode::Backspace => app.terminate_input_backspace(),
                                // Любые символы (включая `q`, `K`, цифры) —
                                // часть набора подтверждения. Esc и Enter
                                // выловлены выше, поэтому здесь они не помешают.
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

            // Activity snapshot.
            res = activity_rx.changed() => {
                match res {
                    Ok(()) => {
                        // `borrow_and_update` отдаёт `Ref<Vec<...>>` и помечает
                        // версию как видимую. Клонируем содержимое — `Ref`
                        // нельзя держать через `.await`.
                        let snapshot = activity_rx.borrow_and_update().clone();
                        app.set_backends(snapshot);
                    }
                    Err(_) => return Ok(()),
                }
            }

            // Locks snapshot.
            res = locks_rx.changed() => {
                match res {
                    Ok(()) => {
                        let snapshot = locks_rx.borrow_and_update().clone();
                        app.set_locks(snapshot);
                    }
                    Err(_) => return Ok(()),
                }
            }

            // Top Queries snapshot.
            res = top_queries_rx.changed() => {
                match res {
                    Ok(()) => {
                        let snapshot = top_queries_rx.borrow_and_update().clone();
                        app.set_top_queries(snapshot);
                    }
                    Err(_) => return Ok(()),
                }
            }

            // Replication snapshot.
            res = replication_rx.changed() => {
                match res {
                    Ok(()) => {
                        let snapshot = replication_rx.borrow_and_update().clone();
                        app.set_replication(snapshot);
                    }
                    Err(_) => return Ok(()),
                }
            }

            // Stats snapshot — push в ring-буфер для sparkline'ов в шапке.
            res = stats_rx.changed() => {
                match res {
                    Ok(()) => {
                        // Stats — Copy, можно `*`-deref'ить из Ref без clone'а.
                        let stats = *stats_rx.borrow_and_update();
                        app.push_stats(stats);
                    }
                    Err(_) => return Ok(()),
                }
            }

            // Action result от executor'а — обновить status-line.
            res = action_result_rx.changed() => {
                match res {
                    Ok(()) => {
                        let snapshot = action_result_rx.borrow_and_update().clone();
                        if let Some(result) = snapshot {
                            app.set_action_result(result);
                        }
                    }
                    Err(_) => return Ok(()),
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
