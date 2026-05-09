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

- [ ] ratatui hello world: alternate screen, raw mode, пустой `Block` с заголовком
- [ ] Event loop через `crossterm::event::EventStream` (async-friendly)
- [ ] Хоткеи: `q` и `Esc` для выхода
- [ ] Статичная таблица с захардкоженными данными
- [ ] Виджет `Table` + `TableState` для выделения строки стрелками
- [ ] Footer с подсказками хоткеев
- [ ] Drop-обёртка, корректно восстанавливающая терминал даже при панике (panic hook + Drop guard)

### Чему учусь

- Архитектура TUI-приложения
- Event loop в async-контексте
- Разница между immediate-mode и retained-mode UI (ratatui — immediate)
- RAII и `Drop` в Rust на практическом примере

### Деливерабл

TUI с заголовком и статичной таблицей, стрелки двигают выделение, `q` закрывает.

---

## Фаза 3. Соединяем pipe с UI (2-3 вечера)

Цель: подружить асинхронный сборщик данных с TUI через каналы.

### Задачи

- [ ] Структура `App { backends: Vec<Backend>, table_state: TableState, ... }`
- [ ] Фоновая `tokio::spawn`-задача опрашивает БД раз в секунду
- [ ] Передача `Vec<Backend>` через `tokio::sync::watch::channel` (нужно только последнее значение)
- [ ] Главный loop: `tokio::select!` между tick (60 fps), input event, обновлением данных
- [ ] Перерисовка по событию или таймеру, не каждый кадр без причины
- [ ] Стрелки двигают выделение реальных бэкендов
- [ ] `Enter` пока ничего не делает (заглушка)
- [ ] Graceful shutdown через `tokio_util::sync::CancellationToken`

### Чему учусь

- `tokio::select!` и его подводные камни (cancellation safety)
- Разница между mpsc / watch / broadcast — почему здесь `watch`
- Graceful shutdown паттерн в async Rust
- Ownership при шеринге state между задачами: message passing > `Arc<Mutex<>>`

### Деливерабл

TUI показывает реальные бэкенды Postgres в реальном времени, обновляется раз в секунду.

---

## Фаза 4. Интерактивная таблица (2-3 вечера)

Цель: превратить таблицу в полезный инструмент с сортировкой, фильтром и detail view.

### Задачи

- [ ] Сортировка по колонкам (хоткеи `s` + выбор колонки, или Shift+стрелки)
- [ ] Фильтр по regex: `/` открывает поле ввода, фильтрует по полю `query`
- [ ] Виджет ввода через крейт `tui-input`
- [ ] Detail view: `Enter` на строке открывает модалку или нижнюю панель с полным текстом запроса, `wait_event`, `backend_xmin`
- [ ] Цветовая подсветка по состоянию: `active` — зелёный, `idle in transaction` — жёлтый, длинные запросы (>10s) — красный
- [ ] `Esc` возвращает из detail view / фильтра в нормальный режим

### Чему учусь

- State machines в TUI: `enum Mode { Normal, Filtering, Detail(i32) }`
- Разделение `App` на компоненты
- Layouts через `Layout::default().constraints(...)`
- Композиция виджетов

### Деливерабл

Можно отфильтровать активные SELECT-ы по regex и посмотреть детали выбранного запроса.

---

## Фаза 5. Несколько источников данных (3-4 вечера)

Цель: расширить мониторинг до нескольких представлений с разными интервалами обновления.

### Задачи

- [ ] Табы вверху: Activity / Locks / Top Queries / Replication
- [ ] Activity — `pg_stat_activity` (1s)
- [ ] Locks — `pg_locks` JOIN `pg_stat_activity` (1s)
- [ ] Top Queries — `pg_stat_statements`, если расширение установлено (10s); если нет — заглушка с инструкцией как поставить
- [ ] Replication — `pg_stat_replication` (5s)
- [ ] Каждый сборщик — свой watch-канал, своя задача
- [ ] В шапке всегда: спарклайны TPS, активных соединений, cache hit ratio
- [ ] Ring buffer последних N значений (`VecDeque<f64>` с `pop_front` при переполнении)
- [ ] Рендер через `Sparkline` widget из ratatui
- [ ] Реорганизация кода: `src/collectors/`, `src/views/`, `src/widgets/`, `src/app.rs`

### Чему учусь

- Trait-объекты vs enum для полиморфизма (`Box<dyn Collector>` vs `enum Collector`)
- Модульная организация Rust-проекта
- Управление множественными tokio-задачами
- Опциональная функциональность (`pg_stat_statements` может отсутствовать) — паттерн с `Option`

### Деливерабл

Переключаешься между табами хоткеями `1`/`2`/`3`/`4`, в каждой — реальные данные, шапка со спарклайнами всегда видна.

---

## Фаза 6. Действия: cancel и terminate (2 вечера)

Цель: добавить безопасное управление бэкендами с защитой от выстрела в ногу.

### Задачи

- [ ] CLI-флаг `--allow-actions`, по умолчанию выключено; без него хоткеи действий не работают
- [ ] Хоткей `c` на выделенной строке → модалка подтверждения → `SELECT pg_cancel_backend($1)`
- [ ] Хоткей `K` (Shift+k) → жёсткая модалка с «type yes to confirm» → `pg_terminate_backend`
- [ ] Своя сессия (`pg_backend_pid()`) подсвечивается серым с меткой `(self)`, хоткеи на ней не работают
- [ ] Audit log через tracing → файл: что/когда/кому отправлено
- [ ] Обработка ошибок прав: красное сообщение в footer, не падать
- [ ] Командный канал: `oneshot` для команды → результат прилетает в UI без блокировки event loop

### Чему учусь

- Modal state в TUI (расширение `enum Mode` из Фазы 4)
- `tokio::sync::oneshot` для команда→ответ
- Неблокирующий вызов БД из event loop
- Разделение «опасных» и «безопасных» режимов через CLI и конфиг

### Деливерабл

С `--allow-actions` можно отменить долгий запрос или прибить idle-in-transaction; своя сессия защищена.

---

## Фаза 7. Конфиг и polish (2 вечера)

Цель: довести до состояния «запускаю каждый день».

### Задачи

- [ ] `~/.config/pgtop/config.toml` с профилями подключений
- [ ] `pgtop bidwise-prod` подхватывает DSN, `read_only`, цвета, интервалы из профиля
- [ ] Парсинг через serde + toml, fallback по приоритетам: CLI flags > env vars > config file > defaults
- [ ] Реализация через `figment`
- [ ] Флаг `--read-only` форсит запрет действий даже при `--allow-actions` (для prod-профилей выставлено всегда)
- [ ] Цветовая тема: dark/light, переключение через конфиг
- [ ] tracing пишет в `~/.local/state/pgtop/pgtop.log` (stdout-то занят TUI)
- [ ] Уровень логирования через `RUST_LOG` (стандарт для tracing-subscriber)

### Чему учусь

- Конфигурация приложения по уровням приоритета
- `figment` как замена ручному merge-у
- XDG Base Directory спецификация
- Логирование в TUI-приложении (stdout недоступен, всё в файл)

### Деливерабл

`pgtop bidwise-prod` просто работает с правильными настройками; `pgtop local --allow-actions` для разработки.

---

## Фаза 8. Multi-connection (2-3 вечера, опционально)

Цель: переключение между несколькими БД в одной сессии TUI.

### Задачи

- [ ] Хоткеи `1`/`2`/`3`/... переключают между подключениями (конфликт с табами решить — например табы на `t`/`T` или цифры на Alt+N)
- [ ] Архитектурно: `Vec<ConnectionState>`, App хранит индекс активного
- [ ] Каждое соединение — свой набор collector-ов и сборщиков статистики
- [ ] Индикатор активного подключения в шапке
- [ ] Состояние «соединение упало, переподключаемся» с retry с exponential backoff

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

- [ ] `cargo dist` для готовых бинарников под Linux/macOS (ARM + x86_64)
- [ ] README со скриншотами и demo-GIF (asciinema или charmbracelet/vhs)
- [ ] `CHANGELOG.md`
- [ ] Опубликовать на GitHub
- [ ] Опубликовать на crates.io
- [ ] Опционально: homebrew tap

### Чему учусь

- Релизный pipeline для Rust-бинарника
- Workflow GitHub Actions для cross-compilation
- Подготовка проекта к публикации (метаданные в `Cargo.toml`, лицензия, README)

### Деливерабл

`brew install pgtop` или curl-installer; страница проекта на GitHub с понятным README.
