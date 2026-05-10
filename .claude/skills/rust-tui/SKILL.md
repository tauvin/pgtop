---
name: rust-tui
description: Use when working on pgtop's TUI components (ratatui widgets, layout, modal states), tokio async tasks (collectors, message passing through watch/mpsc channels), or tokio-postgres queries against pg_stat_* views. Captures architectural patterns specific to this project — how event loops, data flow, and error boundaries are wired.
---

# pgtop — Rust TUI patterns

Skill активируется при работе над:
- ratatui-виджетами, layout'ом, модальными состояниями;
- async-задачами на tokio (сборщики, event loop, graceful shutdown);
- запросами к Postgres через tokio-postgres (`pg_stat_*` представления);
- архитектурой передачи данных между сборщиками и UI (watch/mpsc).

Документ накапливается фазами `docs/ROADMAP.md` — здесь зафиксирован текущий **состоявшийся** набор паттернов и архитектурных решений. Когда добавляешь что-то новое, обновляй соответствующий раздел.

---

## Структура проекта

```
src/
  main.rs        — entry point: connect → spawn collector → TerminalGuard → event loop
  app.rs         — App-state (backends, filtered, table_state, mode, filter, sort)
                   плюс enum'ы Mode/SortBy/SortDirection
  collectors.rs  — фоновые задачи, опрашивающие БД и публикующие в watch
  db.rs          — Backend struct + connect + fetch_backends + raw SQL
  ui.rs          — TerminalGuard (RAII) + render-функции + format-хелперы
```

Дальнейшее планируемое расщепление (Phase 5):
- `collectors/` директория с по-сборщику-на-файл (activity, locks, top_queries, replication);
- `views/` для render-кода разных табов;
- `widgets/` для переиспользуемых ratatui-композиций.

Источник истины по фазам — [`docs/ROADMAP.md`](../../../docs/ROADMAP.md).

---

## Жизненный цикл TUI: `TerminalGuard` (RAII + panic hook)

```rust
pub struct TerminalGuard { terminal: Tui }

impl TerminalGuard {
    pub fn new() -> Result<Self> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        Self::install_panic_hook();
        // ...
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
    fn drop(&mut self) { let _ = restore_disciplines(); }
}
```

**Почему оба механизма:**
- *Drop* запускается на **любом** выходе scope'а (Ok, ?-Err, panic-unwinding) — основной путь восстановления.
- *Panic hook* зовётся **до** unwinding'а и до Drop'ов; без него стек паники ушёл бы в alt-screen и пропал, когда Drop потом восстановит экран.
- Оба идемпотентны (`disable_raw_mode`/`LeaveAlternateScreen` — no-op при повторном вызове), так что двойной запуск безвреден.
- Под `panic = "abort"` Drop не запускается — hook остаётся last resort.

**В `main` — явный `drop(term)` после event loop:** иначе пользователь видит замороженный кадр пока ждём JoinHandle сборщика.

---

## Event loop через `tokio::select!`

Ядро `run_event_loop`:

```rust
loop {
    terminal.draw(|frame| ui::render(frame, app))?;

    tokio::select! {
        // Гард `kind == Press` отсекает дубликаты на kitty-keyboard-protocol.
        maybe_event = events.next() => {
            match maybe_event {
                Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                    // Универсальный Ctrl+C перед mode-dispatch'ем (raw mode
                    // обычно глушит SIGINT-перевод, ловим как KeyEvent).
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL) {
                        return Ok(());
                    }
                    match &app.mode {
                        Mode::Normal => { /* keymap */ }
                        Mode::Detail(_) => { /* keymap */ }
                        Mode::Filter => { /* keymap; forward в tui-input */ }
                    }
                }
                Some(Ok(_)) => {}                  // Resize/Mouse — следующий draw перерисует.
                Some(Err(e)) => return Err(e.into()),
                None => return Ok(()),
            }
        }

        res = data_rx.changed() => {
            match res {
                Ok(()) => app.set_backends(data_rx.borrow_and_update().clone()),
                Err(_) => return Ok(()),  // collector завершился
            }
        }

        _ = tokio::signal::ctrl_c() => return Ok(()),
    }
}
```

**Принципы:**
- *Render-then-wait*: `terminal.draw` вверху loop'а, потом `select!`. Без render-tick'а 60fps — отрисовка только по событию (cheap noop-кадры через ratatui-diff достаточны для UX).
- *Cancel-safe ветки*: `events.next()`, `data_rx.changed()`, `signal::ctrl_c()` — все можно дропать в любой момент без потерь.
- *Тело ветки уже не "гоняется"*: `.await` внутри ветки не прервётся через select. Если нужно — вложенный `select!` с cancel-веткой.

---

## Сборщики данных через `watch::channel`

```rust
pub async fn run_activity_collector(
    client: Client,
    tx: watch::Sender<Vec<Backend>>,
    cancel: CancellationToken,
) {
    let mut ticker = interval(POLL_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        // biased; — cancel ВСЕГДА проверяется первым (иначе tokio
        // случайно перемешивает порядок и cancel может «пропустить ход»).
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            _ = ticker.tick() => {}
        }

        // In-flight fetch тоже cancellable. tokio_postgres::Client::query
        // cancel-safe: drop future оставляет соединение в нормальном
        // состоянии (серверный запрос продолжит выполняться, ответ ignored).
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            r = db::fetch_backends(&client) => r,
        };

        if let Ok(backends) = result {
            // Err от send = все Receiver'ы дропнуты (UI ушёл) — нам тоже пора.
            // Это «signal-of-life» в дополнение к CancellationToken.
            if tx.send(backends).is_err() { break; }
        }
    }
}
```

**Контракт сборщика:**
- Принимает `Client`, `watch::Sender<T>` и `CancellationToken`.
- Опрашивает БД с фиксированным интервалом, публикует **полный snapshot**.
- Два сигнала к выходу: внешний (cancel) и естественный (send returned Err).
- В случае fetch-ошибки молча пропускает тик. Phase 4+ — публиковать `Result<T, E>` в канал, чтобы UI показал ошибку.

**Почему `watch`, а не `mpsc`/`Arc<Mutex>`:**
- *Latest-wins*: UI'у нужен только последний snapshot, история накапливалась бы зря.
- `Receiver::changed()` cancel-safe + трекает «видел/не видел эту версию» из коробки.
- Никаких блокировок через `.await` (типичный footgun `Arc<Mutex>`).

**`borrow_and_update().clone()`:** `Ref<T>` нельзя держать через `.await` (как `RwLockReadGuard`). Сразу клонируем содержимое, освобождаем ref.

---

## Graceful shutdown через `CancellationToken`

`tokio_util::sync::CancellationToken` — shared cancel-флаг с нотификацией. `clone()` шарит underlying state (Arc внутри); `cancel()` идемпотентен и необратим; `cancelled()` cancel-safe для select.

В `main`:
```rust
let cancel = CancellationToken::new();
let collector_handle = tokio::spawn(run_collector(client, tx, cancel.clone()));

let result = run_event_loop(...).await;
drop(term);                      // restore TTY мгновенно
cancel.cancel();                 // сигнал collector'у
let _ = collector_handle.await;  // дождаться реального завершения

result
```

**Порядок важен:**
1. Loop возвращает результат → имеем `result: Result<()>`
2. `drop(term)` — пользователь видит свой shell сразу
3. `cancel.cancel()` — collector получает сигнал
4. `collector_handle.await` — синхронизация на завершении (внутри ~ms)
5. Возврат `result` из main

JoinHandle::await даёт **гарантию** «таска завершилась», в отличие от runtime-abort при drop'е runtime.

---

## Модальные состояния через `enum Mode`

```rust
pub enum Mode {
    Normal,
    Detail(i32),  // pid, не индекс — стабильный к перетасовке
    Filter,       // input/regex живут в App.filter, чтобы переживать выход
}
```

**Принципы:**
- **Хранить стабильные ID, не индексы**: список перетасуется, индекс «уплывёт». pid в `Detail(pid)` устойчив.
- **Match `&app.mode`, не value**: иначе move + потребуется `Copy`. Filter содержит `String` → не Copy.
- **Auto-cleanup в `set_backends`**: если pid в `Detail` исчез из snapshot'а, авто-возврат в `Normal`. Логика в одном месте.
- **Mode-based dispatch хоткеев в main**:
  ```rust
  match &app.mode {
      Mode::Normal => match key.code { ... },
      Mode::Detail(_) => match key.code { ... },
      Mode::Filter => match key.code { ... },
  }
  ```

**Esc контекстный, q универсальный:**
- В `Normal`: q/Esc → quit
- В модалках: q → quit, Esc → close modal
- В `Filter`: Enter → commit, Esc → cancel (clear filter)

---

## Filtered/sorted view через `Vec<usize>`

```rust
pub struct App {
    pub backends: Vec<Backend>,    // полный snapshot
    pub filtered: Vec<usize>,      // индексы прошедших фильтр + отсортированных
    // ...
}

fn recompute_filtered(&mut self) {
    // 1. фильтрация
    self.filtered = self.backends.iter().enumerate()
        .filter(|(_, b)| self.filter.matches(b))
        .map(|(i, _)| i)
        .collect();

    // 2. sort через disjoint field borrows
    let now = Utc::now();
    let by = self.sort.by;
    let dir = self.sort.direction;
    let backends = &self.backends;
    self.filtered.sort_by(|&i, &j| {
        let ord = compare_backends(&backends[i], &backends[j], by, now);
        if dir == SortDirection::Desc { ord.reverse() } else { ord }
    });

    // 3. clamp selection под новую длину
    // ...
}
```

**Принципы:**
- *Один pipeline*: filter + sort + clamp в одной функции, вызываемой из `set_backends`, `cycle_sort_column`, `handle_filter_input`. Никаких рассогласованных промежуточных состояний.
- *Indices, не references*: self-referential structs запрещены. Vec<usize> — простой и эффективный workaround.
- *`TableState.selected` индексирует filtered*, не backends. `visible_backend(idx)` — единственный способ получить Backend «в позиции selected».
- *One `now` per pass*: `Utc::now()` фиксируется один раз на всю сортировку (и на render). Иначе comparator может нарушить транзитивность Ord.

---

## Стилизация и rich text в ratatui

**Иерархия:**
- `Span` — кусок текста с одним `Style`.
- `Line` — последовательность `Span`'ов на одной строке.
- `Text` — несколько `Line`. `Paragraph::new` принимает `Into<Text>`.

**`Stylize`-трейт** даёт builder-методы прямо на `&str`/`String`/`Span`/`Style`:
```rust
"q".bold()                    // Span<'static>
Style::new().fg(Color::Red)   // Style
Style::new().reversed()       // Style с Modifier::REVERSED
" filter: ".dim()             // Span<'static> с DIM
```
Импортировать `use ratatui::style::Stylize` в файлы, где собираешь Span'ы.

**Стили мерджатся**, не заменяют. `Paragraph::style(dim)` + `Span("q").bold()` даёт «тусклый, но жирный q». Идеально для footer'ов: общий dim как фон, отдельные bold-куски на хоткеях.

**`Row::style(...)`** применяется ко всем ячейкам строки. Для per-cell стиля — `Cell::from(text).style(...)`. В pgtop красим всю строку по `state`/duration:
```rust
fn row_style(b: &Backend, now: DateTime<Utc>) -> Style {
    if state == Some("active") && duration > 10 { fg(Red) }
    else if state == Some("active") { fg(Green) }
    else if state.starts_with("idle in transaction") { fg(Yellow) }
    else { default }
}
```
Приоритет: красный > жёлтый > зелёный > default.

**ANSI-цвета** (`Color::Red/Green/Yellow`) — terminal-themed: подстраиваются под цветовую схему пользователя. Для фиксированных — `Color::Rgb(r,g,b)` или `Color::Indexed(n)`.

**Selection styling vs row colors:** `row_highlight_style(reversed())` инвертирует fg/bg. На цветной строке выглядит как «цвет в фон». Если станет некрасиво — переехать на bold+underline для выделения.

---

## Layout и popup-overlays

**`Layout::vertical([...]).areas::<N>()`** — фиксированный массив `[Rect; N]`:
```rust
let [table_area, filter_area, footer_area] = Layout::vertical([
    Constraint::Min(0),     // забирает остаток
    Constraint::Length(1),
    Constraint::Length(1),
]).areas(inner);
```
`.areas::<N>()` лучше `.split()` (которая возвращает `Rc<[Rect]>`): pattern-match даёт compile-time проверку числа constraint'ов.

**Centered popup** через двойной split:
```rust
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let [_, mid_v, _] = Layout::vertical([
        Percentage((100 - percent_y) / 2),
        Percentage(percent_y),
        Percentage((100 - percent_y) / 2),
    ]).areas(area);

    let [_, mid_h, _] = Layout::horizontal([
        Percentage((100 - percent_x) / 2),
        Percentage(percent_x),
        Percentage((100 - percent_x) / 2),
    ]).areas(mid_v);

    mid_h
}
```

**`Clear` widget** перед popup-контентом — «прокалывает дыру», чтобы фоновая таблица не просвечивала:
```rust
frame.render_widget(Clear, popup_area);
frame.render_widget(detail_block, popup_area);
```

**`block.inner(area)` режет область внутри рамки** — иначе контент рисовался бы поверх границы.

---

## Input handling: `tui-input` без crossterm-фичи

`tui-input 0.10` пинит `crossterm = "0.28"`, у нас `crossterm = "0.29"`. Версии semver-несовместимы, типы `Event` не unify. Решение — отказаться от `crossterm`-фичи tui-input и **вручную транслировать `KeyEvent` → `InputRequest`**:

```rust
fn key_to_request(key: KeyEvent) -> Option<InputRequest> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char(c) if !ctrl => Some(InputRequest::InsertChar(c)),
        KeyCode::Char(c) if ctrl => match c {
            'a' | 'A' => Some(InputRequest::GoToStart),
            'e' | 'E' => Some(InputRequest::GoToEnd),
            'u' | 'U' => Some(InputRequest::DeleteLine),
            'w' | 'W' => Some(InputRequest::DeletePrevWord),
            _ => None,
        },
        KeyCode::Backspace => Some(InputRequest::DeletePrevChar),
        KeyCode::Delete => Some(InputRequest::DeleteNextChar),
        KeyCode::Left => Some(InputRequest::GoToPrevChar),
        KeyCode::Right => Some(InputRequest::GoToNextChar),
        KeyCode::Home => Some(InputRequest::GoToStart),
        KeyCode::End => Some(InputRequest::GoToEnd),
        _ => None,
    }
}
```

Бонусы: явный контроль (легко добавить vi-стиль hjkl), независимость от bumps crossterm.

---

## Обработка ошибок

**Слой библиотеки** (`db.rs`, `collectors.rs`): свой `thiserror::Error`-enum:
```rust
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("postgres error: {0}")]
    Postgres(#[from] tokio_postgres::Error),
}
```
`#[from]` генерирует `From<tokio_postgres::Error>` — отсюда работает `?` через границу типов.

**Слой `main`**: `color_eyre::Result<()>` (alias для `Result<(), eyre::Report>`). `eyre::Report` принимает любой `StdError + Send + Sync + 'static`, поэтому `DbError::Postgres(...)` пропускается через `?` в main без явных конверсий.

**`wrap_err`** для добавления контекста:
```rust
enable_raw_mode().wrap_err("enable raw mode")?;
execute!(stdout, EnterAlternateScreen).wrap_err("enter alternate screen")?;
```
В color-eyre отчёте видно цепочку: «failed to start TUI: enter alternate screen: <io error>».

**В Drop — best-effort cleanup**:
```rust
fn drop(&mut self) { let _ = restore_disciplines(); }
```
В Drop нельзя вернуть Result; игнорируем через `let _ =`. Никогда не паниковать в Drop (двойная паника = abort).

**Нет `unwrap`/`expect` в production-пути.** `expect("...")` допустим только для true-инвариантов («это не может случиться, потому что...»).

---

## Соглашения и идиомы проекта

**Disjoint field borrows** — компилятор позволяет одновременно `&self.field_a` (immut) и `&mut self.field_b` (mut). Используем в `recompute_filtered` для sort'а с borrow на `&self.backends` и mut на `self.filtered`.

**Module-level imports** (не внутри функций). Импорт внутри функции — только чтобы разорвать circular import (в Rust почти не встречается).

**`#[rustfmt::skip]` на функции** — стандартный приём для табличных литералов и ASCII-art, чтобы rustfmt не разбивал строки и не ломал колоночное выравнивание.

**`#[allow(dead_code)]` точечно**, не широко. На `Backend` снимаем по мере подключения полей в render. Лучше точечный allow на одно поле/метод, чем на модуль.

**Пары хоткеев `let-chains`**: Rust 1.88+ поддерживает `if let Some(x) = a && let Some(y) = b { ... }`. Clippy-lint `collapsible_if` переписывает вложенные if-let в эту форму.

**`is_some_and(|v| pred(v))`** (Rust 1.70+) вместо `map_or(false, ...)` — короче и читабельнее.

**Один `now` на проход** в render и sort: `let now = Utc::now()` в начале функции, передаём вниз. Не вызываем `Utc::now()` много раз за один кадр/sort.

**Const-массивы вместо `vec![...]`** для статических данных в hot path: `const COLUMNS: [SortBy; 6] = [...]` — компилируется как static, без аллокаций в рантайме.

**Комментарии — Rust-специфичное**, не WHAT. Контекст: автор — Python senior, Rust возобновляет после паузы. Подсвечиваем lifetimes / ownership / trait bounds / `Send + Sync + 'static` для spawn — то, что в Python устроено иначе.
