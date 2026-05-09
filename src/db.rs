//! Низкоуровневый слой работы с Postgres: подключение и выборка `pg_stat_activity`.
//!
//! Здесь живёт всё, что знает про SQL и tokio-postgres. Слои выше (collectors / UI)
//! работают с готовыми структурами `Backend`.

use chrono::{DateTime, Utc};
use tabled::Tabled;
use thiserror::Error;
use tokio_postgres::{Client, NoTls, Row};

/// Ошибки модуля db.
///
/// На Phase 1 — один вариант. `#[from]` ниже генерирует
/// `impl From<tokio_postgres::Error> for DbError`, что и позволяет писать `?`
/// на вызовах tokio-postgres внутри функций модуля.
#[derive(Debug, Error)]
pub enum DbError {
    #[error("postgres error: {0}")]
    Postgres(#[from] tokio_postgres::Error),
}

/// Подмножество полей `pg_stat_activity`, нужное pgtop сейчас.
///
/// Правило nullable: если столбец в Postgres допускает NULL — поле `Option<T>`;
/// если NOT NULL — `T`. `Row::get::<_, T>(...)` **паникует**, если в колонке NULL,
/// а `T` — не `Option`. Поэтому соответствие SQL ↔ модели обязано быть точным.
///
/// `#[derive(Tabled)]` — proc-макрос tabled. На этапе компиляции макрос читает
/// токены этого определения (имена и типы полей, наши `#[tabled(...)]`-аттрибуты)
/// и генерирует `impl Tabled for Backend { fn headers() -> ...; fn fields(&self) -> ...; }`.
/// Вся генерация — compile-time, в рантайме нет ни рефлексии, ни overhead'а.
///
/// Аналогии с Python:
/// - `@attrs.define` / `@dataclass` тоже «генерируют» методы из аннотаций — но в
///   момент создания класса (рантайм). Pydantic делает ещё больше работы при
///   импорте, отсюда заметная стоимость старта.
/// - `attrs.field(metadata={...})` ≈ `#[tabled(...)]` — field-level метаданные
///   для управления генерацией.
/// - serde `#[derive(Serialize)]` устроена точно так же, как Tabled, и тот же
///   паттерн `#[serde(rename = ..., skip, default, ...)]` дублирует наш набор.
///
/// Ключевая разница: proc-макрос видит только синтаксис (TokenStream), а не
/// семантику типов. Он не знает, что `Option<T>` — nullable; он просто вставит
/// в сгенерированный код вызов `fmt_opt_string(&self.field)`, а корректность
/// проверит компилятор. Поэтому для всех `Option<...>` и `DateTime<Utc>` ниже
/// мы обязаны указать `display_with = "fn_name"` — иначе макрос попытается сделать
/// `format!("{}", &self.field)`, а у `Option<String>` нет `Display`-impl.
#[derive(Debug, Clone, Tabled)]
pub struct Backend {
    pub pid: i32,

    #[tabled(rename = "db", display_with = "fmt_opt_string")]
    pub datname: Option<String>,

    #[tabled(rename = "user", display_with = "fmt_opt_string")]
    pub usename: Option<String>,

    #[tabled(rename = "app", display_with = "fmt_opt_string")]
    pub application_name: Option<String>,

    /// `inet` имеет свой бинарный формат; чтобы не тянуть лишний крейт (`cidr`/`ipnet`)
    /// под FromSql, в SQL делаем `client_addr::text` и принимаем как `String`.
    #[tabled(rename = "client", display_with = "fmt_opt_string")]
    pub client_addr: Option<String>,

    /// `backend_start` всегда заполнен у любого backend'а — без `Option`.
    #[tabled(rename = "started", display_with = "fmt_dt")]
    pub backend_start: DateTime<Utc>,

    #[tabled(rename = "tx_start", display_with = "fmt_opt_dt")]
    pub xact_start: Option<DateTime<Utc>>,

    #[tabled(rename = "q_start", display_with = "fmt_opt_dt")]
    pub query_start: Option<DateTime<Utc>>,

    // Поля ниже не отображаются в таблице (`#[tabled(skip)]`) и пока не читаются
    // отдельно — `#[allow(dead_code)]` снимает варнинг, который не имеет смысла
    // на walking skeleton. Уйдут в реальное использование в Phase 4 (detail view)
    // и Phase 5 (locks/deadlocks по xid/xmin).
    #[tabled(skip)]
    #[allow(dead_code)]
    pub state_change: Option<DateTime<Utc>>,

    #[tabled(rename = "wait_type", display_with = "fmt_opt_string")]
    pub wait_event_type: Option<String>,

    #[tabled(rename = "wait", display_with = "fmt_opt_string")]
    pub wait_event: Option<String>,

    #[tabled(display_with = "fmt_opt_string")]
    pub state: Option<String>,

    /// `xid` — 32-битный счётчик транзакций со своим SQL-типом. Кастуем к text,
    /// чтобы не привязываться к доп. feature-флагам tokio-postgres для xid.
    #[tabled(skip)]
    #[allow(dead_code)]
    pub backend_xid: Option<String>,

    #[tabled(skip)]
    #[allow(dead_code)]
    pub backend_xmin: Option<String>,

    #[tabled(display_with = "fmt_query")]
    pub query: Option<String>,

    #[tabled(rename = "type", display_with = "fmt_opt_string")]
    pub backend_type: Option<String>,
}

// --- display-функции для Tabled ---
//
// Сигнатура: `fn(&FieldType) -> impl Display` (на практике мы возвращаем
// `String` — это и проще, и работает с любой версией tabled). Атрибут
// `#[tabled(display_with = "fmt_xxx")]` подставляет вызов `fmt_xxx(&self.field)`
// в сгенерированный код. Имя — путь к функции; пустой путь = тот же модуль.

/// `Option<String>` → строка или прочерк.
fn fmt_opt_string(v: &Option<String>) -> String {
    v.as_deref().unwrap_or("-").to_owned()
}

/// `DateTime<Utc>` → `HH:MM:SS` (компактно для таблицы; полная дата — Phase 2+).
fn fmt_dt(v: &DateTime<Utc>) -> String {
    v.format("%H:%M:%S").to_string()
}

fn fmt_opt_dt(v: &Option<DateTime<Utc>>) -> String {
    v.map(|t| t.format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "-".to_owned())
}

/// SQL-запрос: схлопываем whitespace в один пробел и режем до 60 символов
/// (не байтов — `char_indices` уважает границы UTF-8).
fn fmt_query(v: &Option<String>) -> String {
    let Some(q) = v.as_deref() else {
        return "-".to_owned();
    };
    let one_line: String = q.split_whitespace().collect::<Vec<_>>().join(" ");
    match one_line.char_indices().nth(60) {
        Some((cutoff, _)) => format!("{}…", &one_line[..cutoff]),
        None => one_line,
    }
}

/// `pid <> pg_backend_pid()` отбрасывает нашу собственную сессию,
/// чтобы pgtop не показывал сам себя в списке.
const ACTIVITY_QUERY: &str = "
SELECT
    pid,
    datname,
    usename,
    application_name,
    client_addr::text  AS client_addr,
    backend_start,
    xact_start,
    query_start,
    state_change,
    wait_event_type,
    wait_event,
    state,
    backend_xid::text  AS backend_xid,
    backend_xmin::text AS backend_xmin,
    query,
    backend_type
FROM pg_stat_activity
WHERE pid <> pg_backend_pid()
ORDER BY pid
";

/// Подключается к Postgres по DSN; драйвер соединения детачится в фоновую таску.
///
/// Rust-специфика: `tokio_postgres::connect` возвращает `(Client, Connection)`.
/// `Connection` — самостоятельный Future, который гоняет I/O TCP-сокета;
/// если её не poll'ить параллельно, любой запрос через `Client` встанет навсегда.
/// `move` в замыкании передаёт `connection` в spawn'енную таску по value;
/// bounds `tokio::spawn` (`Future + Send + 'static`) у `Connection` выполняются.
///
/// Walking-skeleton-вариант: задача отдетачена и сама себя логирует при ошибке.
/// В Phase 3 перепишем под `CancellationToken` + `JoinHandle`, чтобы можно было
/// аккуратно остановить драйвер при shutdown.
pub async fn connect(dsn: &str) -> Result<Client, DbError> {
    let (client, connection) = tokio_postgres::connect(dsn, NoTls).await?;

    tokio::spawn(async move {
        if let Err(e) = connection.await {
            tracing::error!("postgres connection driver error: {e}");
        }
    });

    Ok(client)
}

/// Снимает срез `pg_stat_activity` и собирает его в `Vec<Backend>`.
///
/// Принимает `&Client`: внутри `Client` — mpsc-канал до драйвера, так что клиент
/// дешёвый в шеринге (он ещё и `Clone`) и не требует `Arc<Mutex<>>` для
/// конкурентных запросов из разных tokio-задач.
pub async fn fetch_backends(client: &Client) -> Result<Vec<Backend>, DbError> {
    let rows = client.query(ACTIVITY_QUERY, &[]).await?;
    Ok(rows.into_iter().map(row_to_backend).collect())
}

/// Маппит одну строку из `ACTIVITY_QUERY` в `Backend`.
///
/// Про `Row::get`:
/// - принимает либо индекс (`row.get(0)`), либо имя колонки (`row.get("pid")`).
///   Имена устойчивее к перестановке столбцов в SELECT — берём их.
/// - паникует при mismatch типа или NULL в non-`Option`. Поэтому модель `Backend`
///   обязана точно отражать nullable-семантику запроса.
/// - неиспаникнуть-версия — `try_get`, возвращает `Result`. Здесь SQL под нашим
///   контролем, поэтому `get` уместен.
/// - возвращаемый тип `T` выводится из контекста: поле `pid: i32` подсказывает
///   компилятору, что нужен `i32`. Когда контекста нет — turbofish:
///   `row.get::<_, i32>("pid")`.
///
/// Про `chrono::DateTime<Utc>`:
/// - Postgres `timestamptz` хранит абсолютные моменты в UTC. У tokio-postgres
///   есть `FromSql for DateTime<Utc>` под фичей `with-chrono-0_4` (включена
///   в `Cargo.toml`). Без неё пришлось бы парсить байты вручную.
/// - Для `timestamp WITHOUT time zone` использовался бы `chrono::NaiveDateTime`,
///   потому что часового пояса там нет — это другой SQL-тип.
fn row_to_backend(row: Row) -> Backend {
    Backend {
        pid: row.get("pid"),
        datname: row.get("datname"),
        usename: row.get("usename"),
        application_name: row.get("application_name"),
        client_addr: row.get("client_addr"),
        backend_start: row.get("backend_start"),
        xact_start: row.get("xact_start"),
        query_start: row.get("query_start"),
        state_change: row.get("state_change"),
        wait_event_type: row.get("wait_event_type"),
        wait_event: row.get("wait_event"),
        state: row.get("state"),
        backend_xid: row.get("backend_xid"),
        backend_xmin: row.get("backend_xmin"),
        query: row.get("query"),
        backend_type: row.get("backend_type"),
    }
}
