use std::env;

use color_eyre::eyre::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind};
use futures::StreamExt;

mod app;
// db-модуль на Phase 2 практически не используется (только `connect` для smoke
// test'а связи). Полноценный поток данных вернётся в Phase 3 через watch-канал;
// до тех пор подавляем dead_code, чтобы clippy не валился на fetch_backends/Backend.
#[allow(dead_code)]
mod db;
mod ui;

use app::App;

const DEFAULT_DSN: &str = "postgres://pgtop:pgtop@localhost:5433/pgtop";

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    // Phase 2: TUI владеет stdout/stderr внутри alternate screen — обычный
    // tracing-логгер «протекал бы» поверх кадра. tracing вернётся в Phase 7
    // через `tracing-appender` в файл.
    //
    // db::connect остаётся как smoke-test связи (наследие Phase 1). Фоновая
    // connection-таска тихо живёт до выхода процесса; в Phase 3 заведём
    // поток данных через watch::channel.
    let dsn = env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let _client = db::connect(&dsn).await?;

    let mut app = App::new();
    // TerminalGuard — RAII: при выходе из main (через Ok, через ?-ошибку
    // или через unwinding-панику) Drop вернёт TTY в нормальный режим
    // автоматически. Явный restore_terminal больше не нужен.
    let mut term = ui::TerminalGuard::new()?;

    run_event_loop(term.terminal(), &mut app).await
}

/// Async event loop: render → ждём первое событие → реагируем → повторяем.
///
/// Что в `tokio::select!`:
/// - `events.next()` — следующий `Event` от crossterm. `Event` — это enum:
///   `Key(KeyEvent) | Mouse(...) | Resize(u16, u16) | Paste(String) |
///   FocusGained | FocusLost`.
/// - `signal::ctrl_c()` — то же поведение, что в Phase 1: ловим SIGINT
///   независимо от того, отжата ли клавиша Ctrl+C в терминале.
///
/// Хоткеи task 3-5: `q` / `Esc` → выход; стрелки `↑`/`↓` двигают выделение.
/// Остальные клавиши не-выходные — task 4 (Phase 4) добавит сортировку (`s`),
/// фильтр (`/`) и т.д.
async fn run_event_loop(terminal: &mut ui::Tui, app: &mut App) -> Result<()> {
    // EventStream создаётся ОДИН раз: внутри он держит поток-поллер
    // и канал событий. Пересоздавать в loop = терять буферизованные
    // события и платить за spawn потока на каждой итерации.
    let mut events = EventStream::new();

    loop {
        // Closure `|frame| ui::render(frame, app)` реборроу-ет `*app` на время
        // draw'а; после возврата borrow заканчивается, и app снова доступен
        // в match-ветках ниже. Реборроу — автоматическое поведение Rust для
        // mut-references в captures.
        terminal.draw(|frame| ui::render(frame, app))?;

        tokio::select! {
            maybe_event = events.next() => {
                match maybe_event {
                    // Гард `kind == Press` отсекает дубликаты на терминалах
                    // с kitty keyboard protocol (см. task 3).
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                        match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                            KeyCode::Up => app.select_previous(),
                            KeyCode::Down => app.select_next(),
                            _ => {}
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(e.into()),
                    None => return Ok(()),
                }
            }
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}
