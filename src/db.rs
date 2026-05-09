//! Низкоуровневый слой работы с Postgres: подключение и выборка `pg_stat_activity`.
//!
//! Здесь живёт всё, что знает про SQL и tokio-postgres. Слои выше (collectors / UI)
//! работают с готовыми структурами `Backend`.

use chrono::{DateTime, Utc};
use thiserror::Error;
use tokio_postgres::{Client, NoTls, Row};

/// Ошибки модуля db. `#[from]` генерирует `impl From<tokio_postgres::Error>
/// for DbError` — отсюда работает `?` на вызовах tokio-postgres.
#[derive(Debug, Error)]
pub enum DbError {
    #[error("postgres error: {0}")]
    Postgres(#[from] tokio_postgres::Error),
}

/// Подмножество полей `pg_stat_activity`, нужное pgtop.
///
/// Правило nullable: если столбец в Postgres допускает NULL — поле `Option<T>`;
/// если NOT NULL — `T`. `Row::get::<_, T>(...)` **паникует**, если в колонке NULL,
/// а `T` — не `Option`. Поэтому соответствие SQL ↔ модели обязано быть точным.
///
/// `#[allow(dead_code)]`: модель отражает всю выборку; часть полей (например,
/// `application_name`, `client_addr`, `backend_type`) ещё не отрисовывается
/// в табличной view (Phase 3), но появится в detail view (Phase 4). Снимать
/// allow по одному будем по мере подключения полей в render.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Backend {
    pub pid: i32,
    pub datname: Option<String>,
    pub usename: Option<String>,
    pub application_name: Option<String>,
    /// `inet` имеет свой бинарный формат; чтобы не тянуть лишний крейт (`cidr`/`ipnet`)
    /// под FromSql, в SQL делаем `client_addr::text` и принимаем как `String`.
    pub client_addr: Option<String>,
    /// `backend_start` всегда заполнен у любого backend'а — без `Option`.
    pub backend_start: DateTime<Utc>,
    pub xact_start: Option<DateTime<Utc>>,
    pub query_start: Option<DateTime<Utc>>,
    pub state_change: Option<DateTime<Utc>>,
    pub wait_event_type: Option<String>,
    pub wait_event: Option<String>,
    pub state: Option<String>,
    /// `xid` — 32-битный счётчик транзакций со своим SQL-типом. Кастуем к text,
    /// чтобы не привязываться к доп. feature-флагам tokio-postgres для xid.
    pub backend_xid: Option<String>,
    pub backend_xmin: Option<String>,
    pub query: Option<String>,
    pub backend_type: Option<String>,
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
