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

use app::{App, Mode};
use db::Backend;

const DEFAULT_DSN: &str = "postgres://pgtop:pgtop@localhost:5433/pgtop";

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    // Phase 3: TUI владеет stdout/stderr внутри alternate screen. tracing
    // вернётся в Phase 7 через `tracing-appender` в файл.

    let dsn = env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let client = db::connect(&dsn).await?;

    // watch::channel(initial) — latest-wins канал. Sender хранит ровно одно
    // значение, при `send` оно заменяется; Receiver-ы видят новую версию через
    // `.changed().await`. Идеально для мониторинга: UI'у нужен только свежий
    // снапшот, история не интересует.
    //
    // Initial value — пустой Vec: до первого тика collector'а UI рисует пустую
    // таблицу. Первый tick происходит сразу (interval так устроен), задержка
    // до первого реального снапшота = одно сетевое RTT к БД.
    let (data_tx, data_rx) = watch::channel::<Vec<Backend>>(Vec::new());

    // CancellationToken — shared cancel-флаг с нотификацией. clone() шарит
    // underlying state (внутри Arc<...>); cancel с любого клона видят все.
    // main держит оригинал, collector — клон; на shutdown main зовёт cancel,
    // collector видит на ближайшей `.cancelled()` await-точке и завершается.
    let cancel = CancellationToken::new();

    // Сохраняем JoinHandle, чтобы дождаться реального завершения collector'а
    // на shutdown'е (а не просто понадеяться на runtime-abort).
    let collector_handle = tokio::spawn(collectors::run_activity_collector(
        client,
        data_tx,
        cancel.clone(),
    ));

    let mut app = App::new();
    let mut term = ui::TerminalGuard::new()?;
    let loop_result = run_event_loop(term.terminal(), &mut app, data_rx).await;

    // Восстанавливаем терминал ДО shutdown'а collector'а: иначе пользователь
    // видит замороженный кадр пока мы ждём завершение фоновой таски (~до 1с).
    // `drop(term)` явно вызывает Drop, который снимет alt-screen + raw mode.
    drop(term);

    // Просим collector завершиться. Он проснётся на ближайшей
    // `cancel.cancelled()` ветке в своих `select!` и вернётся.
    cancel.cancel();

    // Ждём, что collector действительно завершился. JoinHandle::await отдаёт
    // `Result<(), JoinError>`: Err только при панике в таске. Игнорируем
    // (panic-hook уже отрисовал бы), главное — синхронизация на завершении.
    let _ = collector_handle.await;

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
    mut data_rx: watch::Receiver<Vec<Backend>>,
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
                                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                                KeyCode::Up => app.select_previous(),
                                KeyCode::Down => app.select_next(),
                                KeyCode::Enter => app.on_enter(),
                                KeyCode::Char('/') => app.enter_filter_mode(),
                                KeyCode::Char('s') => app.cycle_sort_column(),
                                KeyCode::Char('S') => app.toggle_sort_direction(),
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

            // `.changed()` резолвится при увеличении внутренней версии (т.е.
            // collector сделал send). Cancel-safe: версия отслеживается на
            // стороне Receiver'а, дроп future не теряет «уже видел/не видел».
            //
            // `Err(_)` — все Sender'ы закрыты (collector упал/завершился).
            // На Phase 3 — просто выходим из UI; в block B будет аккуратнее.
            res = data_rx.changed() => {
                match res {
                    Ok(()) => {
                        // `borrow_and_update()` отдаёт `Ref<Vec<Backend>>` и
                        // помечает «эту версию я видел». Клонируем содержимое,
                        // потому что хотим владеть им в App (и `Ref` нельзя
                        // держать через .await).
                        let snapshot = data_rx.borrow_and_update().clone();
                        app.set_backends(snapshot);
                    }
                    Err(_) => return Ok(()),
                }
            }

            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}
