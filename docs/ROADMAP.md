# pgtop — ROADMAP

Этот документ — план разработки pgtop, разбитый на 10 фаз (0..9). Каждая фаза самостоятельна: после её завершения проект остаётся компилируемым и полезным. По мере выполнения отмечайте чекбоксы как `- [x]`. Это living document — оценки времени и состав задач корректируются по ходу: если что-то выясняется в процессе, правим план.

Проект использует Rust edition 2024 и rust-version 1.95.

---

## Фаза 0. Решения и сетап (1 вечер)

Цель: зафиксировать архитектурные решения и собрать минимальный каркас проекта, чтобы дальше не переделывать.

### Задачи

- [x] Решение: один бинарник, не workspace (разделим позже, когда станет больно)
- [x] Зафиксировать стек: tokio, ratatui + crossterm, tokio-postgres, clap, serde + toml, tracing + tracing-appender, thiserror, color-eyre
- [x] Создать `Cargo.toml` с актуальными версиями зависимостей, edition 2024, rust-version 1.95
- [x] Настроить профиль release с `lto = "thin"`
- [x] Поднять локальный Postgres в Docker (порт 5433, чтоб не конфликтовать с системным)
- [x] Написать `scripts/load.sh` — генератор тестовой нагрузки (несколько SELECT-ов разной длительности, периодические idle-in-transaction)
- [x] Установить `cargo-watch` и `cargo-nextest`
- [x] Настроить pre-commit с `cargo clippy --all-targets -- -D warnings` и `cargo fmt --check`
- [x] Решить: RustRover или VS Code как основной IDE (RustRover — выбор по умолчанию)

### Чему учусь

- Cargo-профили: dev vs release; `lto = "thin"` даёт ~10-20% к скорости рантайма ценой ~2× времени линковки.
- `[dev-dependencies]` ≠ `[dependencies]`: dev-deps подтягиваются только при `cargo test`/examples/benches. `cargo-husky` живёт в dev-деп именно потому, что нужен только разработчику.
- `cargo-husky` использует `build.rs` внутри dev-deps: при первой сборке dev-профиля он копирует `.cargo-husky/hooks/*` в `.git/hooks/`. Сам hook — обычный bash, без сторонних зависимостей.
- `rustfmt --check` жёстко контролирует ширину строк (по умолчанию 100, настраивается через `rustfmt.toml`). Аналог `black` из Python, но обязательный к применению.
- `cargo clippy -- -D warnings` превращает каждый варнинг в ошибку — удобный gate для CI и pre-commit.
- `docker compose` сохраняет данные в named volumes между `down`/`up`, но `down -v` чистит volume.

### Деливерабл

`cargo run` выводит «hello pgtop» и подключается к Postgres.

---

## Фаза 1. Walking skeleton без TUI (1-2 вечера)

Цель: убедиться, что data pipeline от Postgres до экрана работает, без всякого UI.

### Задачи

- [x] async main на tokio
- [x] Подключение к Postgres через tokio-postgres, DSN из env `DATABASE_URL`
- [x] Раз в секунду — `SELECT * FROM pg_stat_activity WHERE pid <> pg_backend_pid()`
- [x] Маппинг строк в структуру `Backend { pid, usename, state, query, query_start, ... }`
- [x] Печать в stdout как таблица (через `tabled` или просто `println!`)
- [x] Корректное завершение по Ctrl+C через `tokio::signal`
- [x] Базовый error handling через thiserror в библиотечном коде и color-eyre в main

### Чему учусь

- `#[tokio::main]` — макрос, разворачивающий обычный `fn main()`, который поднимает tokio-runtime и делает `block_on(async { ... })`.
- `?`-оператор пропихивает ошибку через `From`-impl. `#[derive(thiserror::Error)] #[from]` генерирует этот `From` бесплатно — отсюда «бесшовный» переход между типами ошибок слоёв.
- Разделение error handling в Rust: `thiserror` — типизированные структурированные ошибки в библиотечном слое; `color-eyre` — отчёт с контекстом и backtrace в `main`.
- `tokio_postgres::Row::get`: индекс или имя колонки, тип `T` выводится из контекста, паникует на NULL без `Option<T>` — модель обязана точно отражать nullable-семантику.
- `DateTime<Utc>` для `timestamptz` требует фичи `with-chrono-0_4` в tokio-postgres. Для `timestamp` без TZ — `NaiveDateTime`.
- `tokio::time::interval` vs `sleep` в loop: интервал убирает накопительный дрейф; `MissedTickBehavior::{Burst, Skip, Delay}` — три стратегии для «работа дольше периода». Burst — обычно неподходящий дефолт.
- `tokio::select!` ≠ `Promise.race`: проигравшие Future **дропаются** прямо в await-точке. Отсюда понятие cancellation safety — безопасно ли дропнуть future в любой await без порчи общего состояния.
- `derive(Tabled)` концептуально ≈ `@attrs.define` + serde-стиль декораторов: но генерация — compile-time на TokenStream, без рантайм-рефлексии. Поэтому требуется явный `display_with = "fn"` для `Option<T>` — макрос не «видит» семантику типа.
- `#[tabled(skip)]` не считается «чтением» поля для dead_code-анализа: для скрытых полей пришлось точечно добавить `#[allow(dead_code)]` с комментарием «оживут в Phase 4+».

### Деливерабл

Запущенный pgtop в терминале раз в секунду печатает обновлённый список бэкендов.

---

## Фаза 2. Минимальный TUI (2 вечера)

Цель: освоить базовый event loop ratatui на статичных данных.

### Задачи

- [x] ratatui hello world: alternate screen, raw mode, пустой `Block` с заголовком
- [x] Event loop через `crossterm::event::EventStream` (async-friendly)
- [x] Хоткеи: `q` и `Esc` для выхода
- [x] Статичная таблица с захардкоженными данными
- [x] Виджет `Table` + `TableState` для выделения строки стрелками
- [x] Footer с подсказками хоткеев
- [x] Drop-обёртка, корректно восстанавливающая терминал даже при панике (panic hook + Drop guard)

### Чему учусь

- **Immediate-mode UI**: state живёт у меня (в `App`), виджеты — эфемерные value-объекты, конструируются на каждом кадре. Это не React/GTK с retained-tree; ближе к SwiftUI/Compose.
- **Buffer + diff в ratatui**: каждый `terminal.draw(...)` рисует в `Buffer`, ratatui считает разницу с предыдущим кадром и шлёт в backend только изменения — отсюда дешёвая перерисовка.
- **Layout — pure function**: `Layout::vertical([...]).areas::<N>(rect)` возвращает `[Rect; N]` без какого-либо state. На ресайз ничего не настраиваем — следующий кадр просто решит уравнение под новый `frame.area()`.
- **`.areas::<N>()` vs `.split()`**: с фиксированным массивом и destructuring `let [a, b] = ...` компилятор проверит совпадение числа constraint'ов.
- **Stateful widgets**: `Table` сам stateless, `TableState` хранит `selected`+`offset` между кадрами. Рендер через `render_stateful_widget(widget, area, &mut state)` — ratatui читает state и может его мутировать (например, подвинуть offset под выделение).
- **Span/Line/Text — три уровня rich text**. `Stylize`-трейт даёт builder-методы прямо на `&str`/`Span`/`Style`: `"q".bold()`, `Style::new().dim()` — без него пришлось бы писать длинный `Span::styled(...)`.
- **`Paragraph::style(...)` мерджится со стилями Span'ов**, не заменяет. Идеально для footer'а: общий dim как фон + отдельные bold-куски на хоткеях.
- **`crossterm::event::EventStream`** — async-обёртка над sync `read()`. Под капотом — фоновый поллер (тред / IOCP) + канал. Cancel-safe для `select!`, но **не recreate-safe**: создавать ОДИН раз перед loop'ом, иначе теряются буферизованные события.
- **`KeyEventKind::Press`-фильтр**: на терминалах с kitty keyboard protocol (WezTerm, Kitty, Foot с CSI u) одна клавиша = Press + Release + Repeat. Без фильтра `q` срабатывал бы дважды.
- **Match-гарды и or-patterns** делают код хоткеев компактным: `Some(Ok(Event::Key(key))) if key.kind == Press`, `KeyCode::Char('q') | KeyCode::Esc`. Когда гард ложный — match идёт к следующей ветке, как будто pattern не совпал.
- **`tokio::select!` ≠ `Promise.race`**: проигравшие Future дропаются — отсюда понятие cancellation safety. На практике: `EventStream::next` и `signal::ctrl_c()` обе cancel-safe; долгие операции внутри ветки уже не прерываются (фаза 3 — обернём в `CancellationToken`).
- **RAII в Rust = Drop**: запускается на конце scope'а — нормальный `return`, ранний `?`, panic-unwinding. Даёт «using/with»-семантику без `try/finally`.
- **Drop + panic hook оба нужны**. Hook вызывается **до** unwinding'а и Drop'ов. Если hook напечатает стек панике, пока терминал в alt-screen — стек уйдёт в alt-screen и пропадёт. Поэтому hook **тоже** делает cleanup, а Drop остаётся как страховка для штатного выхода. Идемпотентность операций (`LeaveAlternateScreen`, `disable_raw_mode`) делает двойной вызов безвредным.
- **`panic = "abort"` отключает Drop**, поэтому panic hook нельзя выкидывать как «дублирующий» — он остаётся последним рубежом.
- **Реборроу `&mut` в closure**: `terminal.draw(|frame| render(frame, app))` — closure захватывает `app: &mut App` и реборроу-ет `*app` на время вызова. После возврата borrow заканчивается, `app` снова доступен.
- **`#[rustfmt::skip]` на функции** — стандартный приём для табличных литералов и ASCII-art, чтобы rustfmt не разбивал строки на multi-line и не ломал колоночное выравнивание.

### Деливерабл

TUI с заголовком и статичной таблицей, стрелки двигают выделение, `q` закрывает.

---

## Фаза 3. Соединяем pipe с UI (2-3 вечера)

Цель: подружить асинхронный сборщик данных с TUI через каналы.

### Задачи

- [x] Структура `App { backends: Vec<Backend>, table_state: TableState, ... }`
- [x] Фоновая `tokio::spawn`-задача опрашивает БД раз в секунду
- [x] Передача `Vec<Backend>` через `tokio::sync::watch::channel` (нужно только последнее значение)
- [x] Главный loop: `tokio::select!` между tick (60 fps), input event, обновлением данных
- [x] Перерисовка по событию или таймеру, не каждый кадр без причины
- [x] Стрелки двигают выделение реальных бэкендов
- [x] `Enter` пока ничего не делает (заглушка)
- [x] Graceful shutdown через `tokio_util::sync::CancellationToken`

### Чему учусь

- **`tokio::sync::watch` — latest-wins канал.** Sender хранит ровно одно значение, при `send` оно заменяется. Receiver видит обновления через `.changed().await` (cancel-safe) и читает через `.borrow()` или `.borrow_and_update()`. Идеально для мониторинга: «свежее всегда лучше старого».
- **Сравнение каналов.** `mpsc` накапливает историю — для UI получился бы лаг. `broadcast` — много подписчиков с буфером, тоже лишнее. `watch` — один писатель, много читателей, без буферизации. `Arc<Mutex<Vec<Backend>>>` — без built-in нотификации, придётся будить через Notify, плюс риск hold lock через .await (deadlock).
- **`Ref<T>` нельзя держать через `.await`.** `borrow_and_update()` возвращает `Ref` (как `RwLockReadGuard`); если задержать через async-yield, заблокируешь Sender. Паттерн — сразу `.clone()` и отпустить.
- **`CancellationToken` (`tokio_util`)** — shared cancel-флаг с нотификацией. `clone()` шарит underlying state (Arc внутри); `.cancel()` идемпотентен и необратим; `.cancelled()` cancel-safe для select!. Лучше чем Notify (нет «уже cancelled?» state) и лучше чем oneshot (одноразовый, неудобно делить).
- **`biased;` в `select!`.** По умолчанию tokio случайно перемешивает порядок веток для fairness. Для cancellation это плохо: cancel может «пропустить ход». `biased;` фиксирует порядок на декларационный — критично для shutdown'а.
- **Nested `select!` для cancellation in-flight операции.** Ветка `tokio::select! { _ = cancel.cancelled() => break, r = fetch_backends(...) => r }` позволяет дропнуть fetch на cancel. `tokio_postgres::Client::query` cancel-safe: drop future оставляет соединение в нормальном состоянии (серверный запрос продолжит выполняться, ответ ignored).
- **Spawn + JoinHandle для синхронизации завершения.** До Phase 3 `tokio::spawn` без сохранения handle = «фоновая задача без явного завершения», runtime аборт'ит на shutdown. Сохранение handle и `.await` даёт *гарантию* «таска действительно завершилась».
- **Сигнал «UI ушёл» через `tx.send(...).is_err()`.** Естественный double-check к CancellationToken: если все Receiver'ы дропнуты, send fails — collector выходит сам, даже без явного cancel. На shutdown оба пути сходятся к одному.
- **Один `now` на кадр.** В `render_table` считаем `Utc::now()` один раз и передаём в каждый `backend_to_row` — иначе разные строки показывали бы duration от микро-разных моментов времени.
- **Auto-clamp выделения в `set_backends`.** Если collector прислал список короче, селекшен мог уйти за len; если стал пустым — селекшен должен стать None. Инвариант после `set_backends`: selected либо None (пустая таблица), либо валидный индекс.
- **Drop ordering для UX.** Явный `drop(term)` после `run_event_loop` восстанавливает терминал ДО shutdown'а collector'а — пользователь сразу возвращается к shell, не смотрит замороженный кадр пока ждём JoinHandle.
- **`#[allow(dead_code)]` на структуре vs полях.** Backend моделирует всю SELECT-выборку из `pg_stat_activity`; не все поля сейчас отрисовываются, но все populated одинаково — так что allow на структуре, а не на каждом неиспользуемом поле. Снимется само, когда подключим detail view (Phase 4).
- **Метод-заглушка как extension point.** `App::on_enter()` сейчас no-op, но это уже финальная форма для main: Phase 4 правит только тело метода, не event loop. Лучше чем `KeyCode::Enter => {}` inline в main.
- **Module split раньше необходимости.** Создал `collectors.rs` для одного collector'а — выглядит overkill, но Phase 5 явно зовёт разделять (`activity`, `locks`, `top_queries`, `replication`). Заранее обозначить границу легче, чем потом резать `db.rs` пополам.

### Деливерабл

TUI показывает реальные бэкенды Postgres в реальном времени, обновляется раз в секунду.

---

## Фаза 4. Интерактивная таблица (2-3 вечера)

Цель: превратить таблицу в полезный инструмент с сортировкой, фильтром и detail view.

### Задачи

- [x] Сортировка по колонкам (хоткеи `s` + выбор колонки, или Shift+стрелки)
- [x] Фильтр по regex: `/` открывает поле ввода, фильтрует по полю `query`
- [x] Виджет ввода через крейт `tui-input`
- [x] Detail view: `Enter` на строке открывает модалку или нижнюю панель с полным текстом запроса, `wait_event`, `backend_xmin`
- [x] Цветовая подсветка по состоянию: `active` — зелёный, `idle in transaction` — жёлтый, длинные запросы (>10s) — красный
- [x] `Esc` возвращает из detail view / фильтра в нормальный режим

### Чему учусь

- **`enum Mode` как FSM модальных состояний.** `Normal | Detail(pid) | Filter` + match по `&app.mode` (не value, чтобы не требовать `Copy` — `Filter` содержит `String`). Каждый mode имеет свой keymap, свой render-overlay, свой footer-hint.
- **Хранить стабильные ID, не индексы.** `Detail(pid)` устойчив к перетасовке snapshot'а; индекс бы «уплыл» при следующем обновлении. Цена — `iter().find()` по pid на render, но при <50 backend'ах копейки.
- **Auto-close modals в `set_backends`.** Если pid в Detail исчез — режим возвращается в Normal автоматически, в одном месте логики.
- **Esc — контекстный, q — универсальный.** В Normal: q/Esc → quit. В модалках: q → quit, Esc → close. В Filter: Enter → commit, Esc → cancel.
- **`Stylize`-трейт даёт builder-методы прямо на `&str`/`Style`/`Span`.** `"q".bold()` вместо `Span::styled("q", Style::new().add_modifier(Modifier::BOLD))`. Импортировать в файлы, где собираешь Span'ы.
- **Стили мерджатся, не заменяют.** `Paragraph::style(dim) + Span.bold()` = тусклый, но жирный текст. Используем для footer'а: общий dim + bold-хоткеи.
- **Row.style для всей строки vs Cell.style для одной ячейки.** Для категоризации row'ов по state — Row.style. Цвета terminal-themed (`Color::Red` — ANSI 1, подстраивается под схему).
- **`Clear` widget для popup'а.** Без `Clear` фон таблицы просвечивает в дырах между Span'ами popup'а. С `Clear` — чистая «дыра» под контент.
- **Centered popup через двойной `Layout::vertical/horizontal` split** с pattern destructure `let [_, mid, _] = ...areas(...)`.
- **`block.inner(area)` режет область внутри рамки.** Без него Block-content рисуется поверх границы.
- **Span/Line/Text — иерархия rich text.** Span = атом, Line = строка из Span'ов, Text = несколько Line. Paragraph принимает `Into<Text>`.
- **`tui-input` + ручной маппинг `KeyEvent → InputRequest`.** Когда зависимости пинят разные мажоры crossterm (несовпадение типов `Event`), отказываемся от crossterm-фичи tui-input и пишем свой переводчик клавиш в `InputRequest`. Бонус: явный контроль над поддержкой emacs/readline/vi-биндингов.
- **`regex::RegexBuilder` для опциональных параметров.** `RegexBuilder::new(pat).case_insensitive(true).build()` — case-insensitive search для UX.
- **`Vec<usize>` как «view» поверх `Vec<Backend>`.** Self-referential structs запрещены, индексы — простой workaround. Filtered/sorted view рассчитывается в одной функции `recompute_filtered`, вызываемой из всех точек изменения (set_backends, cycle_sort_column, handle_filter_input).
- **Disjoint field borrows.** `let backends = &self.backends; self.filtered.sort_by(|&i, &j| compare(&backends[i], &backends[j]))` — один immutable + один mutable borrow разных полей одного struct'а компилятор пропускает.
- **`Option::cmp` и tuple Ord.** `Option<String>::cmp` — лексикографически (None < Some); `(a1, a2).cmp(&(b1, b2))` — сначала по первому, при равенстве — по второму. Бесплатный multi-key sort без лишних аллокаций.
- **`Ordering::reverse()` для desc-сортировки.** Один comparator + флаг направления, применяемый к результату. Не пишем «desc-comparator» отдельно.
- **Один `now` на render/sort pass.** `Utc::now()` дрейфует на наносекунды; для транзитивности Ord и консистентности duration в разных строках фиксируем один раз.
- **Const-массивы для статических данных.** `const COLUMNS: [SortBy; 6] = [...]` — компилируется как static, без аллокаций. Привычка для hot-path кода.
- **`is_some_and` (Rust 1.70+)** короче `map_or(false, ...)`.
- **`if let ... && let ...` (Rust 1.88+)** заменяет вложенный `if let { if let { ... } }`. Clippy-lint `collapsible_if` переписывает автоматически.
- **`#[rustfmt::skip]` на функции** — для табличных литералов с колоночным выравниванием.
- **Универсальный Ctrl+C хендлер перед mode dispatch.** В raw mode терминальный драйвер обычно не транслирует Ctrl+C в SIGINT → `tokio::signal::ctrl_c()` срабатывает только от внешнего `kill`. Ловим явно как `Char('c') + CONTROL` для универсального quit'а из любого режима.

### Деливерабл

Можно отфильтровать активные SELECT-ы по regex и посмотреть детали выбранного запроса.

---

## Фаза 5. Несколько источников данных (3-4 вечера)

Цель: расширить мониторинг до нескольких представлений с разными интервалами обновления.

### Задачи

- [x] Табы вверху: Activity / Locks / Top Queries / Replication
- [x] Activity — `pg_stat_activity` (1s)
- [x] Locks — `pg_locks` JOIN `pg_stat_activity` (1s)
- [x] Top Queries — `pg_stat_statements`, если расширение установлено (10s); если нет — заглушка с инструкцией как поставить
- [x] Replication — `pg_stat_replication` (5s)
- [x] Каждый сборщик — свой watch-канал, своя задача
- [x] В шапке всегда: спарклайны TPS, активных соединений, cache hit ratio
- [x] Ring buffer последних N значений (`VecDeque<f64>` с `pop_front` при переполнении)
- [x] Рендер через `Sparkline` widget из ratatui
- [x] Реорганизация кода: `src/collectors/`, `src/views/`, `src/widgets/`, `src/app.rs`

### Чему учусь

- **JIT module split.** Один `collectors.rs` → `collectors/{mod.rs, activity.rs}` ровно когда появился второй collector (а не «впрок»). То же для views/ и widgets/. Цена ошибки несимметрична: преждевременный split → лишние файлы; поздний → монолит на 800 строк, который потом неудобно резать.
- **Multi-source через параллельные watch-каналы.** Каждый collector — свой `watch::channel<T>`, своя `tokio::spawn`-таска, своя ветка в `select!`. На shutdown — `tokio::join!(handle1, handle2, …)` ждёт всех. Альтернативы (один общий enum-канал, FuturesUnordered) — overengineering для 5 источников.
- **`tokio_postgres::Client` НЕ `Clone`** (несмотря на `Arc<...>` внутри — public API не экспонирует). Чтобы получить true-параллелизм между collector'ами, делаем **отдельный** `db::connect` на каждого — отдельное TCP-соединение и driver-таска. `Arc<Client>` сериализовал бы запросы через один driver.
- **Stateful collector pattern.** Stats-collector держит `let mut prev_xacts: Option<i64>` локально в функции — переживает между итерациями loop'а. Никаких внешних структур, scope сам себе ownership.
- **Three-state snapshot enum** (`Loading | ExtensionMissing | Available`). Лучше `Option<T>`: явные семантические состояния «ещё не загрузили» vs «фича недоступна» vs «вот данные» — UI рисует разный фолбэк для разных причин.
- **Empty-state UX.** Пустой Vec + просто пустая таблица = «pgtop сломан?». Отдельная render-ветка с информативным сообщением — стандартный приём, цена пара десятков строк.
- **`Sparkline` widget принимает `&[u64]`** (не `f64`). Для дробных метрик (TPS<10) — масштабировать `(v * 10.0) as u64` перед передачей; для процентов фиксировать `.max(100)`. Auto-max не искажает форму графика, но игнорирует «нолики» при truncate.
- **`VecDeque<T>` для ring-buffer.** `push_back` + `pop_front` оба O(1) (circular buffer). У `Vec` `pop_front` = O(n), для high-frequency обновлений неприемлемо.
- **Disjoint field borrows в render.** `match &app.top_queries { Available(q) => render(q, &mut app.top_queries_table_state) }` — компилятор пропускает immut + mut на разные поля. Передавать `&mut app` целиком в helper уже не получится.
- **Postgres SQL-quirks при работе с tokio-postgres**:
  - `SUM(bigint)` возвращает `numeric` (для overflow-safety), не bigint. Для tokio-postgres без `rust_decimal`-крейта нужно явно `::int8`.
  - Литерал `100.0` — `numeric` по умолчанию. Для float8 → `100.0::float8` или весь expression `(...)::float8`.
  - `pg_lsn` и `interval` — постгрес-специфичные типы; в SQL кастуем `::text` / `EXTRACT(EPOCH FROM ...)::float8` чтобы не подключать дополнительные feature-флаги tokio-postgres.
- **NULL на проде, NOT NULL в docs.** `pg_stat_activity.backend_start` по доке обязателен, на реальных prod-серверах встречается NULL у служебных процессов (checkpointer/walreceiver/walwriter). Default-стратегия для всех timestamp-полей в БД-моделях: `Option<DateTime<Utc>>`. Цена ошибки несимметрична: лишний `Option` = `unwrap_or` в render; missing `Option` = runtime panic.
- **Feature detection через `EXISTS`-подзапрос.** `SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'pg_stat_statements')` — стандартный паттерн «доступна ли фича», вместо try-and-parse-error. Один лишний round-trip, но детерминистично.
- **Connection string: URL vs libpq key=value.** `tokio-postgres` понимает оба формата. Для паролей со спецсимволами key=value (`host=... password='секрет#с@спецсимволами'`) спасает от URL-encoding (`%23%40` etc.) — читабельнее, меньше шансов ошибиться.
- **`Tabs` widget**: `.select(idx).highlight_style(...).divider(...)` — идиоматическая подсветка активного таба. ratatui сам разруливает spacing.
- **Layout`.areas::<N>()` с N constraint'ами** даёт compile-time проверку количества Rect'ов: добавишь constraint, забудешь поправить pattern — ошибка компиляции, а не runtime-IndexOutOfBounds.
- **Production-realities** (вне Rust, но критично):
  - SSH `AllowTcpForwarding no` — частая запретилка на prod-серверах. Альтернатива: собирать pgtop прямо на whitelist'нутой машине.
  - Пароли со спецсимволами требуют либо URL-encoding (`%XX` для каждого символа), либо libpq key=value формата.
  - `pg_stat_statements` в prod-проде — осознанный rollout: `shared_preload_libraries` требует restart Postgres. Сначала бэкап конфига → проверить наличие `pg_stat_statements.so` → правка → restart → `CREATE EXTENSION` per-database.

### Деливерабл

Переключаешься между табами хоткеями `1`/`2`/`3`/`4`, в каждой — реальные данные, шапка со спарклайнами всегда видна.

---

## Фаза 6. Действия: cancel и terminate (2 вечера)

Цель: добавить безопасное управление бэкендами с защитой от выстрела в ногу.

### Задачи

- [x] CLI-флаг `--allow-actions`, по умолчанию выключено; без него хоткеи действий не работают
- [x] Хоткей `c` на выделенной строке → модалка подтверждения → `SELECT pg_cancel_backend($1)`
- [x] Хоткей `K` (Shift+k) → жёсткая модалка с «type yes to confirm» → `pg_terminate_backend`
- [x] Своя сессия подсвечивается серым (через `application_name = 'pgtop'`), хоткеи на ней не работают
- [x] Audit log через tracing → файл: что/когда/кому отправлено
- [x] Обработка ошибок прав: цветное сообщение в status-line, не падать
- [x] Командный канал: `mpsc` + `watch` для команды и результата, event loop не блокируется

### Чему учусь

- **Asymmetric channels: `mpsc` for commands, `watch` for results.** Команды важны каждая (FIFO, ни одна не теряется) → `mpsc`; результаты — важна последняя (latest-wins, без накопления) → `watch`. Один канал на оба не подходит, потому что семантика разная.
- **`UnboundedSender::send` синхронный** — не возвращает Future, не нужно await'ить. Это критично для event loop'а: команда уходит мгновенно, render не блокируется. На bounded mpsc был бы `try_send` с возможностью drop'а при переполнении; для нашего юзкейса (1 команда раз в несколько секунд) unbounded безопасен.
- **Type-yes anti-fool design.** Pattern для destructive actions: пока ввод не равен conventional confirmation-string («yes», «delete», имя ресурса) — Enter no-op. Убирает класс accidental-press ошибок, стандарт для kubectl/terraform/AWS-CLI. В Rust проще всего держать набираемый текст в самом enum-варианте: `Mode::ConfirmTerminate(pid, String)`, мутировать через `if let ... = &mut self.mode`.
- **Mutating String внутри enum-варианта.** `&mut self.mode` → match'имся на `&mut Mode` → bind `text: &mut String`, который можем `.push(c)` / `.pop()`. Естественный pattern для FSM с встроенной mutable data; не надо выносить state наружу в отдельные поля.
- **`KeyCode::Char('K')` matches Shift+k.** crossterm на большинстве терминалов выдаёт **уже заглавную букву** (terminal-level case folding), а не `Char('k') + Modifier::SHIFT`. Сматчить только на код символа без проверки модификатора — переносимо.
- **`SET application_name = 'pgtop'`** для self-detection лучше, чем коллекция собственных PID'ов. Метка наследуется любыми будущими соединениями автоматически, в отличие от списка из 5-6 pid'ов, который пришлось бы поддерживать вручную. Plus DBA на проде сразу понимает «это monitor».
- **Audit log через `tracing` с custom target.** `tracing::info!(target: "audit", action = "cancel", pid = 12345, "...")` — структурированное событие с фильтруемым target'ом. Юзер настраивает `RUST_LOG=audit=info` отдельно от остальной телеметрии. tracing-appender пишет в файл через background-writer (non-blocking), `WorkerGuard` держится до конца main для flush.
- **`ActionResult.outcome: Result<bool, String>`** — три состояния: `Ok(true)` (успешно послано), `Ok(false)` (нет такого pid или нет permission'ов — Postgres не различает их в return-value), `Err(s)` (SQL-ошибка). UI красит по семантике: ✓ зелёный / ⚠ жёлтый / ✗ красный.
- **Auto-close pid-bound модалок при исчезновении backend'а** — расширили pattern с Detail (Phase 4) на ConfirmCancel и ConfirmTerminate. Единая логика в `set_backends`, перебирающая все pid-варианты Mode.
- **`clap` derive Parser** — declarative CLI: одна struct + `#[arg]` атрибуты, и `Cli::parse()` разруливает help/validation/parsing. Минимум boilerplate'а, compile-time проверка типов. Phase 7 расширит структуру под profile-name + --read-only.
- **`tracing-appender::non_blocking` + `WorkerGuard` ownership.** Неинтуитивный момент — guard НЕ background-task'а, это RAII-handle, при дропе которого background-writer flush'нет буфер и закроется. Underscore-prefix (`_log_guard`) чтобы compiler не варнил unused; держать в main до самого конца.
- **`KeyCode::Char(c)` в ConfirmTerminate-keymap.** Любые символы (буквы, цифры, пунктуация) идут в push. `Esc` / `Enter` / `Backspace` обрабатываются явными `KeyCode`-ветками выше match'а. Никаких regex-фильтров на input — гейт только в `text == "yes"` сравнении при Enter.
- **`Span::raw(...).style(...)`** — обычный builder vs Stylize-trait `.bold()`/`.red()`. Используем builder когда нужно полный `Style` (с custom fg+bg+modifier'ами); Stylize-методы — для одного modifier'а коротко. Оба идиоматичны, выбор по контексту.
- **Visual hierarchy опасности через цвет.** Cancel — жёлтая рамка/prompt, Terminate — красная + явный «destructive» текст. Цвет — основной канал коммуникации в TUI; иконки/иероглифы вторичны. Пользователь видит цвет до того, как читает текст.
- **Production-realities Phase 6:** при rolling out actions на проде нужны два независимых разрешения — `--allow-actions` локально (защита от misclick'а в pgtop'е) **и** Postgres-permissions для самого юзера. `pg_cancel_backend` доступна owner'у backend'а или `pg_signal_backend`-роли; `pg_terminate_backend` — superuser'у или той же роли. Получишь `Ok(false)` при insufficient privilege — это нормальный путь, не SQL-ошибка.

### Деливерабл

С `--allow-actions` можно отменить долгий запрос или прибить idle-in-transaction; своя сессия защищена.

---

## Фаза 7. Конфиг и polish (2 вечера)

Цель: довести до состояния «запускаю каждый день».

### Задачи

- [x] `~/.config/pgtop/config.toml` с профилями подключений
- [x] `pgtop bidwise-prod` подхватывает DSN, `read_only`, цвета, интервалы из профиля
- [x] Парсинг через serde + toml, fallback по приоритетам: CLI flags > env vars > config file > defaults
- [x] Реализация через `figment`
- [x] Флаг `--read-only` форсит запрет действий даже при `--allow-actions` (для prod-профилей выставлено всегда)
- [x] Цветовая тема: dark/light, переключение через конфиг
- [x] tracing пишет в `~/.local/state/pgtop/pgtop.log` (stdout-то занят TUI)
- [x] Уровень логирования через `RUST_LOG` (стандарт для tracing-subscriber)

### Чему учусь

- **Layered config через figment + ручной layering.** figment отлично грузит TOML с line:column-ошибками; для env-CLI-overrides на nested-структурах (HashMap profiles) проще ручной layering в `Resolved::from_layers`. Гибрид: figment даёт структурированный parse, ручная логика — приоритеты и fallback'и.
- **`#[serde(default)]` для дружелюбности конфига.** Без него отсутствие секции `[profiles]` или поля `read_only = false` = parse error. С ним пустой / минимальный конфиг работает. Стандарт для optional-полей в YAML/TOML-конфигах.
- **Resolved struct vs Config struct.** Config — что лежит в файле (Option-fields, defaults). Resolved — финальные runtime-значения после layering'а. Чистая функция `from_layers(config, cli...) -> Resolved` тестируема, стейтлесс. Cleaner, чем Config с set-методами и Option'ами на каждом use-site.
- **`Profile::read_only = true` как «sticky off» seal.** OR двух источников (`cli_read_only || profile.read_only`); CLI не может «снять» read_only с профиля. Анти-fool: запустил `pgtop prod --allow-actions` по привычке — actions всё равно выключены, потому что профиль так настроен.
- **`dirs::state_dir()` cross-platform asymmetry.** XDG state — Linux-only стандарт, macOS/Windows возвращают `None`. Fallback chain `state_dir → data_local_dir → home_dir/.local/state → cwd` через `Option::or_else` — функциональный pattern lazy-cascading'а. Бонус: `PGTOP_LOG_DIR` env override для CI/контейнеров.
- **`RUST_LOG` через `EnvFilter::try_from_default_env`** работает «бесплатно» — никакой кастомной парсинг-логики не нужно. `RUST_LOG=audit=info,pgtop=debug` фильтрует target'ы независимо.
- **Theme as Copy struct.** Семантические цвета (success/warning/danger/muted) в одной маленькой структуре с `Copy`. Pass by value в render-функции — cheap, никакой Arc-семантики. `Theme::dark()` и `Theme::light()` — `const fn` constructors, оптимально.
- **«Light theme» minimum viable.** ANSI Red/Green/Yellow одинаково смотрятся на любом фоне (terminal сам рендерит). Реальная разница — `muted` (DarkGray vs Gray). Не стоит изобретать дюжину разных оттенков, когда terminal уже разруливает 80% задачи.
- **Configurable intervals через resolved-passing.** Добавить `poll_interval: Duration` параметр в каждый `run_*_collector`, удалить `const POLL_INTERVAL`. main.rs передаёт `resolved.intervals.activity` etc. при `tokio::spawn`. Никакого глобального состояния, легко тестируется.
- **CLI positional + flags вместе.** clap legkо позволяет `pgtop [PROFILE] [--flags]`: profile — `Option<String>` без `#[arg]`-атрибутов, флаги — с `#[arg(long)]`. Если профиль указан но не найден в конфиге — error с `available: [...]` listing'ом, classical UX-pattern.
- **Title-bar как mode-indicator.** ` pgtop · prod · RO — Activity (45 backends) ` — компактная строка, в которой видны: имя профиля (если активен), read-only индикатор (sticky-off для actions), таб + counter. UX: пользователь видит «где я» и «что мне можно» в одном месте. Особенно полезно когда несколько pgtop-сессий открыты в разных терминалах под разные профили.
- **`config.example.toml` рядом с `Cargo.toml`.** Конкретный документ-tutorial, который пользователь копирует в `~/.config/pgtop/config.toml` и редактирует. Inline комментарии в TOML — лучшая документация конфига; README может ссылаться на этот файл.

### Деливерабл

`pgtop bidwise-prod` просто работает с правильными настройками; `pgtop local --allow-actions` для разработки.

---

## Фаза 8. Multi-connection (2-3 вечера, опционально)

Цель: переключение между несколькими БД в одной сессии TUI.

### Задачи

- [x] Хоткеи `1`/`2`/`3`/... переключают между подключениями (конфликт с табами решён — переключение на `Alt+N`, табы остались на цифрах)
- [x] Архитектурно: `Vec<ConnectionState>`, App хранит индекс активного
- [x] Каждое соединение — свой набор collector-ов и сборщиков статистики (через shared mpsc fan-in с conn_idx)
- [x] Индикатор активного подключения в шапке (`· N/M`)
- [x] Состояние «соединение упало, переподключаемся» с retry с exponential backoff (500ms → 30s cap, индикатор `· connecting #N…`)

### Чему учусь

- Управление несколькими наборами async-задач
- Reconnection logic
- Более сложное управление UI-состоянием

### Деливерабл

Одной сессией pgtop можно мониторить prod / staging / local одновременно.

---

## Фаза 9. Релиз (1 вечер)

Цель: сделать проект устанавливаемым и видимым.

### Задачи

- [x] `cargo dist` для готовых бинарников под Linux/macOS (ARM + x86_64) — `dist-workspace.toml` + `.github/workflows/release.yml` собирают на тег `v*`
- [x] README — структура, фичи, install, usage, hotkeys, config, dev. Demo-GIF: TODO-плейсхолдер, добавится после первой записи через vhs/asciinema
- [x] `CHANGELOG.md` в формате Keep a Changelog
- [x] Опубликовать на GitHub (`tauvin/pgtop`)
- [x] Опубликовать на crates.io (`pgtop` v0.1.1)
- [ ] Опционально: homebrew tap (после первого релиза, через `dist init` с `homebrew-tap`)

### Чему учусь

- Релизный pipeline для Rust-бинарника
- Workflow GitHub Actions для cross-compilation
- Подготовка проекта к публикации (метаданные в `Cargo.toml`, лицензия, README)

### Деливерабл

`brew install pgtop` или curl-installer; страница проекта на GitHub с понятным README.

---

## Фаза 10. Production-readiness (2-3 вечера)

Цель: закрыть пробелы, которые мешают использовать pgtop в реальной работе с managed-Postgres'ами и не сломать функциональность будущими правками.

### Задачи

- [x] **TLS-поддержка** через `tokio-postgres-rustls` — `sslmode` в DSN, поддержка `disable/prefer/require/verify-ca/verify-full`. По умолчанию `prefer` (как у `psql`).
- [x] **Тесты на чистую логику**: 28 unit-тестов на Filter, SortBy/SortDirection, compare_backends, форматтеры (duration, query truncation, wait, lag).
- [ ] **Snapshot-тесты UI** через `insta` + `ratatui::TestBackend` — отложено: Activity/Detail рендер использует `Utc::now()`, нужен time-injection рефактор для воспроизводимых снапшотов.
- [x] **MSRV CI job** — отдельная джоба в `ci.yml`, билдит на `rustc 1.88`.
- [x] **Demo `.tape`** — `docs/demo.tape` со сценарием записи через `vhs`. Сама запись — когда у автора будет живой Postgres с трафиком.

### Чему учусь

- TLS-конфигурация и rustls-стек (`PgConnectOptions`, system root certs, SNI)
- Snapshot-тестирование TUI (`insta` review workflow)
- Cargo features (TLS опционален? фиксируем как обязательный?)

### Деливерабл

pgtop подключается к managed-Postgres'ам без хаков, изменения покрыты тестами, CI ловит MSRV-регрессии.

---

## Фаза 11. Расширение покрытия pg_stat_* (3-4 вечера)

Цель: догнать `pgcenter`/`pg_top` по аналитической глубине — больше табов, больше срезов.

### Задачи

- [x] **`pg_stat_database` tab (`5`)** — backends, commits, rollbacks, cache hit %, temp bytes, deadlocks per database. Default 5s poll.
- [x] **`pg_stat_user_tables` tab (`6`)** — top 50 by dead-tuple count, with live/dead tuple counts, dead %, last vacuum/analyze ("5m"/"2h"/"3d"), seq vs idx scan counts. Default 10s poll.
- [x] **Waits histogram tab (`7`)** — sampling-based aggregate of `(wait_event_type, wait_event)` from latest activity snapshot. No extra SQL.
- [x] **EXPLAIN-popup на Activity** — `e` на выбранной строке → popup с `EXPLAIN <query>` через ad-hoc connection. EXPLAIN без ANALYZE — read-only safe.
- [x] **Долгие-запросы alert** — `slow_query_threshold_secs` в конфиге (default 30s). Активные запросы > threshold подсвечены red+bold; в title-баре Activity появляется `⚠ N slow`.

### Чему учусь

- Sampling vs counter-based метрики (wait events)
- Тонкости `EXPLAIN` без ANALYZE (когда безопасно, когда нет)
- Динамическая генерация таб-системы (текущая хардкоднута на 4)

### Деливерабл

pgtop отвечает на 80% вопросов DBA без переключения на `psql` / `pgcenter`.

---

## Фаза 12. UX и distribution (2 вечера)

Цель: убрать шероховатости в ежедневном использовании и расширить каналы установки.

### Задачи

- [x] **Мышь** — scroll-wheel для пагинации списков (click-to-select отложен — нужен row hit-test против layout'а таба, малая marginal value).
- [ ] **Keymap в config** — отложено: требует Command-based dispatch refactor; default-биндинги (htop-like) подходят большинству.
- [x] **Persist UI state** — `dirs::data_local_dir()/pgtop/state.toml`; restore tab/filter/sort на старте.
- [x] **Поиск-и-прыжок (`g`-mark)** — `g` на Activity → input pid → курсор на эту строку.
- [x] **Homebrew tap** — `dist-workspace.toml` обновлён с `installers = ["shell", "homebrew"]` + `tap = "tauvin/homebrew-pgtop"`. Юзер должен создать пустой репо `tauvin/homebrew-pgtop` и `HOMEBREW_TAP_TOKEN` secret в основном репо для авто-публикации.
- [ ] **AUR package** — отложено, требует AUR-аккаунта и `pgtop-bin` PKGBUILD.
- [x] **Docker image** — `Dockerfile` (multi-stage, `rust:1.90 → debian:slim`) + `.github/workflows/docker.yml` собирает `ghcr.io/tauvin/pgtop` multi-arch (amd64+arm64) на тег `v*`.
- [x] **Nix flake** — `flake.nix` через crane, `nix run github:tauvin/pgtop`.

### Чему учусь

- Mouse events в crossterm (button/scroll, mod-keys, mouse-leak в alternate screen)
- TOML-парсинг сложных pattern'ов (keybind как строка → `KeyEvent`)
- Distribution-формат-zoo (homebrew vs AUR vs Nix vs Docker)

### Деливерабл

pgtop устанавливается одной строкой на любой популярной платформе, ежедневное использование без раздражений.

---

## Фаза 13. Технический долг и рефакторинг

Цель: привести структуру в порядок. Последовательность поставлена так, чтобы каждый шаг разблокировал следующий с меньшим риском (рекомендация архитектурного ревью v0.1.4).

### Задачи (по порядку)

1. [x] **Time-injection для рендера** — `Utc::now()` сейчас вызывается независимо в `views/activity.rs`, `views/tables.rs`, `ui::count_slow`. Передать `now: DateTime<Utc>` параметром через `ui::render`. Минимальный диф, **разблокирует следующий пункт**.
2. [x] **Snapshot-тесты UI** — `insta` + `ratatui::TestBackend` для Activity / Locks / Detail / ConfirmTerminate / Databases / Tables. После #1 рендер детерминирован. Делает следующие шаги безопасными.
3. [x] **Split `app.rs` (~1213 строк) по data ownership, не per-noun** — `app/mod.rs` (App, Mode, ExplainPopup, top-level методы), `app/connection.rs` (ConnectionState, Filter, Sort, StatsHistory, WaitRow), `app/tab.rs` (Tab, SortBy, SortDirection). 8 файлов по 60 строк было бы хуже текущего монолита.
4. [x] **Per-mode keymap handlers в `run_event_loop`** — выделить `handle_normal_mode` / `handle_filter_mode` / `handle_confirm_cancel` / `handle_confirm_terminate` / `handle_explain` / `handle_jump`. Чистый refactor без новой архитектуры. После split'а проще найти dispatch'ам дом.
5. [ ] **Action enum + Keymap** — gated на реальный запрос на custom keybinds. Не строить на спекуляции.
6. [x] **Rate-based метрики в Databases-табе** — TPS per database через stateful collector с prev-snapshot. Независимо от остального.
7. [x] **Borrowed `Row<'a>` в render-path** — `backend_to_row` теперь возвращает `Row<'a>` с `Cow<'a, str>`-ячейками; usename/state/wait/query берут borrow вместо `.clone()`. Disjoint-field-borrow `&conn.backends + &mut conn.table_state` обходит self-method блокировку. Применено к Activity (1 Hz рендер); прочие view'ы оставлены на `Row<'static>` — там аллокации не на hot path.
8. [ ] **`Tab` / `SortBy` через `strum`** — `EnumIter`/`EnumString`/`Display` derive'ы вместо тройных match-ей `index/from_index/from_id/label/from_label`. Косметика.

### Чему учусь

- Чувство меры: когда расщеплять монолитный match (per-mode handlers), а когда вводить полноценный command pattern (Action enum)
- Time-injection как предпосылка для детерминистичных UI-тестов
- Splitting монолитного файла по data ownership vs per-noun — почему первое часто лучше

### Деливерабл

Event-loop ~50 строк, Activity/Tables/Detail покрыты snapshot-тестами, app.rs распилен по data ownership, render-path без аллокаций на hot loop.

---

## Фаза 14. Hardening (production-readiness)

Цель: закрыть фактические корректностные/безопасностные дыры, найденные ревью трёх инженеров (rust-engineer, code-quality-reviewer, software architect) поверх 0.1.4. Делается до того как pgtop пойдёт в реальные команды.

### Critical (security / correctness)

- [x] **TLS `verify-ca` хостнейм-проверка** — `VerifyMode` enum (None/ChainOnly/Full); `ChainOnlyVerifier` оборачивает `WebPkiServerVerifier` и игнорирует `NotValidForName` ошибки.
- [x] **EXPLAIN: `statement_timeout` + реальный cancel** — `SET statement_timeout = '5s'` перед EXPLAIN; `Client::cancel_token().cancel_query(NoTls)` на cancel; per-popup `CancellationToken` отменяется на `set_active`/`close_modal`.
- [x] **Audit log hardening** — двух-sink layered subscriber (app + audit), daily rotation, mode 0600 на Unix, log-dir 0700.

### Important (latent risk / UX)

- [ ] **Bounded mpsc + drop-oldest** — отложено. На текущей нагрузке (2N msg/sec при N≤5) латентный риск, не реальный. Делать когда (а) пользователи начнут жаловаться на память, или (б) появится реальная multi-conn нагрузка ≥10 conn'ов.
- [x] **Silent `Err(_) => {}` в коллекторах** — `tracing::warn!` с `collector`/`conn_idx`/`error` во всех 7 collector'ах.
- [x] **`last_action_result` per-connection** — переехало с `App` на `ConnectionState`; результаты не теряются на background-conn.
- [x] **Shutdown timeout на `join_all`** — `tokio::time::timeout(2s, ...)` + warn-лог на превышение.
- [x] **`DEFAULT_DSN` → None + clear error** — убран dev-default; сообщение указывает три способа (CLI / env / profile) и путь к config'у.

### Quality / polish

- [x] **Filter** — теперь `(query, usename, state, datname)`; +2 теста.
- [x] **`Resolved::from_layers` unit tests** — 7 тестов на priority chain и `actions_allowed && !read_only`.
- [x] **MSRV CI добавить `cargo test`** — `cargo test` теперь и на rustc 1.88.
- [ ] **`rewrite_verify_sslmode` через DSN parsing** — отложено. Текущий `contains` работает на корректных DSN; substring-в-password — теоретическая угроза.
- [ ] **Мелкие гнильцы**: `next_tab().unwrap()`, panic hook double-install, stale `#[allow(dead_code)]`. Низкий приоритет.
- [ ] **Connection pooling per profile** — отложено, требует архитектурного решения (`bb8`/`deadpool` с ~2 conn'ами на profile vs текущая модель «1 conn на collector»).

### Чему учусь

- Channel sizing и backpressure стратегии в TUI (latest-wins vs FIFO mpsc)
- Custom rustls `ServerCertVerifier` для разных уровней проверки (chain-only vs chain+hostname)
- Postgres-специфичный cancellation: PG-сторонний запрос отменяется только через отдельный backend и `pg_cancel_backend`
- Tracing layered architecture: разные sink'и для audit vs debug через `filter::Targets`
- File mode permissions в Rust (`std::os::unix::fs::OpenOptionsExt::mode`)

### Деливерабл

pgtop устойчив к: misbehaving DB (slow queries не блокируют EXPLAIN connection, tx errors не теряются молча), slow renderer (backpressure), dropped TLS hostname check (verify-ca теперь корректен), action results from background connections (per-conn). Audit log приватен и rotate'ится. CI ловит MSRV-регрессии в тестах.
