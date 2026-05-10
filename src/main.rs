use std::env;

use color_eyre::eyre::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

mod app;
mod collectors;
mod db;
mod ui;
mod views;
mod widgets;

use app::{App, Mode, Tab};
use db::{Backend, Lock, Replica, Stats, TopQueriesSnapshot};

const DEFAULT_DSN: &str = "postgres://pgtop:pgtop@localhost:5433/pgtop";

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    // Phase 3: TUI владеет stdout/stderr внутри alternate screen. tracing
    // вернётся в Phase 7 через `tracing-appender` в файл.

    let dsn = env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
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

    // CancellationToken — shared cancel-флаг с нотификацией. clone() шарит
    // underlying state (внутри Arc<...>); cancel с любого клона видят все.
    // main держит оригинал, collector'ы — клоны; на shutdown main зовёт cancel,
    // оба collector'а видят на ближайшей `.cancelled()` await-точке.
    let cancel = CancellationToken::new();

    let activity_handle = tokio::spawn(collectors::run_activity_collector(
        client_activity,
        activity_tx,
        cancel.clone(),
    ));
    let locks_handle = tokio::spawn(collectors::run_locks_collector(
        client_locks,
        locks_tx,
        cancel.clone(),
    ));
    let top_queries_handle = tokio::spawn(collectors::run_top_queries_collector(
        client_top_queries,
        top_queries_tx,
        cancel.clone(),
    ));
    let replication_handle = tokio::spawn(collectors::run_replication_collector(
        client_replication,
        replication_tx,
        cancel.clone(),
    ));
    let stats_handle = tokio::spawn(collectors::run_stats_collector(
        client_stats,
        stats_tx,
        cancel.clone(),
    ));

    let mut app = App::new();
    let mut term = ui::TerminalGuard::new()?;
    let loop_result = run_event_loop(
        term.terminal(),
        &mut app,
        activity_rx,
        locks_rx,
        top_queries_rx,
        replication_rx,
        stats_rx,
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
async fn run_event_loop(
    terminal: &mut ui::Tui,
    app: &mut App,
    mut activity_rx: watch::Receiver<Vec<Backend>>,
    mut locks_rx: watch::Receiver<Vec<Lock>>,
    mut top_queries_rx: watch::Receiver<TopQueriesSnapshot>,
    mut replication_rx: watch::Receiver<Vec<Replica>>,
    mut stats_rx: watch::Receiver<Stats>,
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

            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}
