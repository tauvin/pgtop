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
  main.rs                — entry point: clap CLI parse → init audit log → 6×connect →
                           6×spawn → TerminalGuard → event loop
  app.rs                 — App-state (per-tab data + Mode/Tab/Filter/Sort + StatsHistory
                           + actions_allowed + last_action_result)
  db.rs                  — Backend/Lock/Replica/TopQuery/Stats + fetch_* + raw SQL +
                           Backend::is_self() (через application_name)
  actions.rs             — ActionCommand + ActionResult + run_action_executor
                           (mpsc commands → SQL → watch results + audit log)
  collectors/
    mod.rs               — re-exports
    activity.rs          — pg_stat_activity (1s)
    locks.rs             — pg_locks JOIN pg_class (1s)
    top_queries.rs       — pg_stat_statements с extension-detection (10s)
    replication.rs       — pg_stat_replication (5s)
    stats.rs             — TPS/conns/cache hit с stateful prev-snapshot (1s)
  views/
    mod.rs               — re-exports
    activity.rs          — табличный render с self-row подсветкой (DarkGray)
    locks.rs             — Locks с подсветкой waiting
    top_queries.rs       — three-state: Loading/ExtensionMissing/Available
    replication.rs       — empty-state + table
  widgets/
    mod.rs               — re-exports
    detail.rs            — centered popup для Activity Detail
    confirm.rs           — confirm-modals (cancel/terminate) с цветовой иерархией
    filter_line.rs       — статус-строка filter / action-result (mutual exclusion)
    footer.rs            — mode/tab-aware хоткеи
    sparklines.rs        — header-полоса TPS/conns/cache
    tabs.rs              — tab bar
  ui.rs                  — TerminalGuard (RAII) + top-level render dispatch
```

**Когда расщеплять модуль на директорию.** JIT-принцип: монолит `X.rs` живёт пока в нём один тип/одна сущность. Появляется второй (locks-collector рядом с activity-collector) — режется на `X/{mod.rs, foo.rs, bar.rs}`. Преждевременный split (когда «когда-нибудь будет много файлов») = просто лишние папки.

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

## Multi-source pipeline: несколько collector'ов параллельно

Когда источников становится больше одного (Phase 5 — Activity, Locks, Top Queries, Replication, Stats), архитектура расширяется механически:

```rust
// main.rs
// 1. Отдельный connect на каждого collector'а — true параллелизм.
//    `tokio_postgres::Client` НЕ Clone (хотя Arc'нут внутри),
//    и `Arc<Client>` сериализовал бы запросы через один driver.
let client_activity = db::connect(&dsn).await?;
let client_locks = db::connect(&dsn).await?;
let client_top_queries = db::connect(&dsn).await?;
// ...

// 2. Свой watch::channel<T> на каждый snapshot-тип.
let (activity_tx, activity_rx) = watch::channel::<Vec<Backend>>(Vec::new());
let (locks_tx, locks_rx) = watch::channel::<Vec<Lock>>(Vec::new());
let (top_queries_tx, top_queries_rx) =
    watch::channel::<TopQueriesSnapshot>(TopQueriesSnapshot::Loading);
// ...

// 3. spawn'аем все, держим JoinHandle'ы.
let activity_handle = tokio::spawn(run_activity_collector(client_activity, activity_tx, cancel.clone()));
let locks_handle    = tokio::spawn(run_locks_collector(client_locks, locks_tx, cancel.clone()));
// ...

// 4. select! с веткой на каждый канал.
loop {
    terminal.draw(|f| ui::render(f, app))?;
    tokio::select! {
        // events.next() / signal::ctrl_c() / data branches
        res = activity_rx.changed() => {
            if res.is_ok() {
                app.set_backends(activity_rx.borrow_and_update().clone());
            }
        }
        res = locks_rx.changed() => { /* same shape */ }
        // ...
    }
}

// 5. На shutdown — `tokio::join!` ждёт всех.
let _ = tokio::join!(activity_handle, locks_handle, top_queries_handle, /* ... */);
```

**Почему НЕ один общий enum-канал** (`watch::channel<Snapshot>` где `enum Snapshot { Activity(...), Locks(...) }`):
- Любой send заменяет последнее значение → теряются обновления других источников.
- Нужны были бы `mpsc` + queue, что усложняет «latest-wins»-семантику.
- Отдельные каналы — proper isolation, явный `select!`-pattern.

**Stateful collector** (когда нужен diff между snapshot'ами, как TPS у stats):
```rust
pub async fn run_stats_collector(client, tx, cancel) {
    let mut prev_xacts: Option<i64> = None;
    let mut prev_time: Option<Instant> = None;
    loop {
        // tick + cancel select
        let raw = fetch_raw_stats(&client).await?;
        let now = Instant::now();
        let tps = match (prev_xacts, prev_time) {
            (Some(px), Some(pt)) => (raw.xacts - px) as f64 / now.duration_since(pt).as_secs_f64(),
            _ => 0.0,  // первый tick — нет prev
        };
        prev_xacts = Some(raw.xacts);
        prev_time = Some(now);
        tx.send(Stats { tps, ... });
    }
}
```

State держится локально в функции — переживает между итерациями loop'а, scope-ownership, никаких внешних структур.

---

## Опциональные фичи через three-state snapshot

Когда фича может **отсутствовать** на сервере (extension не установлен, конфиг disabled, etc.) — чище explicit FSM, чем `Option<Vec<T>>`:

```rust
#[derive(Debug, Clone)]
pub enum TopQueriesSnapshot {
    Loading,            // initial, до первого poll'а
    ExtensionMissing,   // pg_stat_statements не установлен
    Available(Vec<TopQuery>),
}
```

Различие важное:
- `Option::None` склеивает «ещё не загрузили» и «недоступно».
- UI должен рисовать **разный** fallback: «Loading…» vs инструкция как поставить.

**Detection в fetch-функции:** EXISTS-подзапрос вместо try-and-handle-error:
```rust
let row = client.query_one(
    "SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'pg_stat_statements')",
    &[],
).await?;
let exists: bool = row.get(0);
if !exists { return Ok(TopQueriesSnapshot::ExtensionMissing); }
// ... reading from pg_stat_statements
```

Парсить error message «relation does not exist» хрупко (формат меняется между версиями); EXISTS — детерминистично, +1 round-trip за poll.

**Empty-state UX:** для случая «фича доступна, но данных пока нет» (например `pg_stat_replication` без реплик) — отдельная render-ветка с info-message. Silent empty-table = «pgtop сломан?»; явное «No active replicas. ...» — пользователь понимает.

---

## Sparkline header через `VecDeque` ring-buffer

Header-метрики (TPS, active conns, cache hit) — push'атся в bounded ring-buffer:

```rust
pub struct StatsHistory {
    pub tps: VecDeque<f64>,
    pub conns: VecDeque<u32>,
    pub cache_hit: VecDeque<f64>,
    pub current: Option<Stats>,
}

const STATS_HISTORY_LEN: usize = 60;  // минута истории при 1Hz

fn push_bounded<T>(buf: &mut VecDeque<T>, value: T) {
    buf.push_back(value);
    if buf.len() > STATS_HISTORY_LEN {
        buf.pop_front();
    }
}
```

`VecDeque` критично — `pop_front` амортизированно O(1), у `Vec` это O(n). Для high-frequency обновлений правильный инструмент.

**`Sparkline` widget принимает `&[u64]`**, не f64:
- Для `f64` метрик с малыми значениями (TPS<10) — масштабировать `(v * 10.0) as u64` перед передачей. Auto-max не искажает форму графика, но без scaling'а дробные значения truncate'ятся в нули.
- Для процентов фиксировать `.max(100)` чтобы 99% не выглядело как «забит» когда диапазон стабилизировался.

```rust
let data: Vec<u64> = history.tps.iter().map(|&v| (v * 10.0) as u64).collect();
Sparkline::default()
    .block(Block::default().borders(Borders::TOP).title(format!(" TPS: {:.1} ", current)))
    .data(&data)
    .style(Style::new().fg(Color::Cyan));
```

Title в block'е — компактный способ показать **текущее** значение рядом с историей, без отдельного label widget'а.

---

## Action executor: команды через mpsc, результаты через watch

Destructive-операции (cancel/terminate в Phase 6) исполняются в **отдельной**
spawn'нутой таске со своим Postgres-соединением. Архитектура:

```rust
// main.rs
let client_actions = db::connect(&dsn).await?;

let (action_tx, action_rx) = mpsc::unbounded_channel::<ActionCommand>();
let (action_result_tx, action_result_rx) = watch::channel::<Option<ActionResult>>(None);

let action_handle = tokio::spawn(actions::run_action_executor(
    client_actions,
    action_rx,
    action_result_tx,
    cancel.clone(),
));

// Event loop:
//  - press 'c' → app.try_open_confirm_cancel()
//  - press Enter в ConfirmCancel → action_tx.send(ActionCommand::Cancel { pid })
//  - executor исполняет SQL, шлёт ActionResult через watch
//  - select! ловит action_result_rx.changed() → app.set_action_result(...)
//  - filter_line рисует цветной status (✓/⚠/✗)
```

**Asymmetric channels**:
- `mpsc::UnboundedSender` для **команд**: каждая команда важна (FIFO, ни одна не теряется), `send()` синхронный — event loop не блокируется.
- `watch::channel<Option<ActionResult>>` для **результатов**: важна последняя (latest-wins), `Receiver::changed()` встаёт в `select!` рядом с другими каналами.

Один общий канал на оба не подходит — семантика разная.

**Executor task**:
```rust
pub async fn run_action_executor(
    client: Client,
    mut commands_rx: mpsc::UnboundedReceiver<ActionCommand>,
    result_tx: watch::Sender<Option<ActionResult>>,
    cancel: CancellationToken,
) {
    loop {
        let cmd = tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            c = commands_rx.recv() => match c {
                Some(c) => c,
                None => break,  // все Sender'ы закрыты
            },
        };
        let outcome = execute(&client, &cmd).await;
        log_audit(&cmd, &outcome);
        let _ = result_tx.send(Some(ActionResult { command: cmd, outcome, at: Utc::now() }));
    }
}
```

Тот же `biased; cancel | tick`-pattern, что в collector'ах. CancellationToken
для graceful-shutdown'а.

**Self-protection через `application_name`**. В `db::connect`:
```rust
let _ = client.execute("SET application_name = 'pgtop'", &[]).await;
```

Все 6 наших соединений (5 collector'ов + executor) автоматически получают
эту метку. `Backend::is_self() -> bool` = `application_name == Some("pgtop")`.
Проще, чем хранить список своих PID'ов вручную — новое соединение
наследует автоматически.

**Action outcome — three-state Result**:
```rust
pub struct ActionResult {
    pub command: ActionCommand,
    pub outcome: Result<bool, String>,
    pub at: DateTime<Utc>,
}
```
- `Ok(true)` — success: signal послан.
- `Ok(false)` — нет такого pid'а ИЛИ нет permission'ов (Postgres не различает в return-value).
- `Err(s)` — SQL-ошибка (соединение, синтаксис, и т.п.).

UI красит: ✓ зелёный / ⚠ жёлтый / ✗ красный. Не падает на Err.

---

## Type-yes confirmation для destructive actions

Для cancel — обычный yes/no popup на Enter/Esc. Для **terminate** — type-yes:
пользователь должен ввести точное слово `yes`, иначе Enter no-op. Стандарт
для destructive ops в CLI tools (kubectl, terraform).

Implementation: набираемый текст лежит **внутри Mode-варианта**:
```rust
pub enum Mode {
    // ...
    ConfirmCancel(i32),                  // pid
    ConfirmTerminate(i32, String),       // pid + набираемый текст
}
```

Мутация через паттерн с `&mut self.mode`:
```rust
pub fn terminate_input_push(&mut self, c: char) {
    if let Mode::ConfirmTerminate(_, text) = &mut self.mode {
        text.push(c);
    }
}

pub fn try_confirm_terminate(&mut self) -> Option<i32> {
    if let Mode::ConfirmTerminate(pid, text) = &self.mode
        && text == "yes"
    {
        let pid = *pid;
        self.close_modal();
        return Some(pid);
    }
    None  // text != "yes" — Enter ничего не делает
}
```

Главное — `&mut Mode` в pattern даёт `&mut String` внутри варианта.
Никаких отдельных полей для input'а — state живёт прямо в FSM.

Цветовая иерархия для подтверждения:
- Cancel (мягкое): жёлтая рамка popup'а, prompt жёлтый/зелёный.
- Terminate (destructive): красная рамка + текст «This is destructive».
- Prompt становится зелёным когда `text == "yes"` — визуальный сигнал «теперь Enter сработает».

---

## Audit log через `tracing` target

События cancel/terminate / любые admin-actions пишутся в файл с custom
target'ом — фильтруется через `RUST_LOG=audit=info` отдельно от runtime-шума.

```rust
// actions.rs
fn log_audit(cmd: &ActionCommand, outcome: &Result<bool, String>) {
    match outcome {
        Ok(true) => tracing::info!(target: "audit",
            action = cmd.label(), pid = cmd.pid(),
            "action executed successfully"),
        Ok(false) => tracing::warn!(target: "audit",
            action = cmd.label(), pid = cmd.pid(),
            "action returned false (no such backend or insufficient permission)"),
        Err(e) => tracing::error!(target: "audit",
            action = cmd.label(), pid = cmd.pid(), error = %e,
            "action failed with SQL error"),
    }
}
```

В `~/.pgtop/pgtop.log` (`tracing-appender::rolling::never`):
```
2026-05-10T12:34:56Z  INFO audit: action executed successfully action="cancel" pid=12345
```

`WorkerGuard` от `non_blocking()` держится в main до конца — иначе background-writer
не успеет flush'нуть последние записи.

```rust
fn init_audit_log() -> Result<WorkerGuard> {
    let file = tracing_appender::rolling::never(&log_dir, "pgtop.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file);
    tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_env_filter(EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("info")))
        .with_ansi(false)  // никаких ANSI-кодов в файле — grep дружелюбнее
        .init();
    Ok(guard)
}

// main:
let _log_guard = init_audit_log()?;  // underscore — Rust-сигнал «нужен ради Drop»
```

---

## Postgres SQL-quirks под tokio-postgres

Без extra-крейтов (`rust_decimal`, `cidr`, `ipnet`) tokio-postgres не умеет десериализовать ряд Postgres-типов. Решается явными кастами в SQL:

| Postgres-тип / выражение | Rust-проблема | Cast |
|---|---|---|
| `SUM(bigint)` | возвращает `numeric`, не `bigint` | `SUM(...)::int8` |
| `COUNT(*)` | возвращает `bigint` (часто хочется `i32`) | `COUNT(*)::int4` |
| `100.0 * x` | numeric литерал → результат numeric | `(100.0 * x)::float8` |
| `inet` (client_addr) | требует `cidr`-крейт | `client_addr::text` |
| `xid` (backend_xid) | требует feature-флага | `backend_xid::text` |
| `pg_lsn` (sent_lsn) | свой формат | `sent_lsn::text` |
| `interval` (replay_lag) | свой формат | `EXTRACT(EPOCH FROM replay_lag)::float8` |

Принцип: **каст в целевой Rust-тип в SQL**, а не подключать новые крейты ради одного поля.

---

## NULL-safety в Postgres-моделях

**По дефолту все `Option<T>` для `timestamp`/`text`/`int`-полей**, даже если docs обещают NOT NULL. Реалии прода:
- `pg_stat_activity.backend_start` — формально NOT NULL, на проде встречается NULL у служебных backend'ов (checkpointer, walreceiver, walwriter).
- `pg_stat_activity.pid` — действительно NOT NULL для прикладных backend'ов, но NULL для prepared transactions.
- Field rename'ы между мажорами (`pg_stat_statements.total_time` → `total_exec_time` в PG13).

Цена ошибки несимметрична:
- Лишний `Option` → один `unwrap_or_else(em_dash)` в render.
- Missing `Option` → **runtime panic** в `Row::get` (не компиляционная ошибка).

Стратегия: всегда `Option<T>` для nullable, и снимать `Option` точечно только когда **явно подтверждено** (на бенчмарке / явной WHERE-фильтрации) что NULL невозможен.

---

## Модальные состояния через `enum Mode`

```rust
pub enum Mode {
    Normal,
    Detail(i32),                        // pid — стабильный к перетасовке
    Filter,                             // input/regex живут в App.filter
    ConfirmCancel(i32),                 // pid + Enter/Esc keymap
    ConfirmTerminate(i32, String),      // pid + набираемый text + type-yes
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
