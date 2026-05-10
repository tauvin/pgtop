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
#[derive(Debug, Clone)]
pub struct Backend {
    pub pid: i32,
    pub datname: Option<String>,
    pub usename: Option<String>,
    pub application_name: Option<String>,
    /// `inet` имеет свой бинарный формат; чтобы не тянуть лишний крейт (`cidr`/`ipnet`)
    /// под FromSql, в SQL делаем `client_addr::text` и принимаем как `String`.
    pub client_addr: Option<String>,
    /// По доке `pg_stat_activity.backend_start` NOT NULL, но в реальности
    /// бывает NULL для некоторых служебных backend'ов (checkpointer,
    /// walreceiver, walwriter в момент инициализации). На проде встречается.
    pub backend_start: Option<DateTime<Utc>>,
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

impl Backend {
    /// Это одно из соединений pgtop'а? Проверяется через `application_name`,
    /// который мы выставляем в `db::connect`. Используется для:
    /// - визуальной серой подсветки таких строк в Activity-табе;
    /// - блокировки cancel/terminate-actions на самих себя.
    pub fn is_self(&self) -> bool {
        self.application_name.as_deref() == Some("pgtop")
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
/// Сразу же помечает соединение `application_name = 'pgtop'` — для self-detection
/// в Activity-табе и для DBA-видимости («это наш monitor»).
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

    // `application_name` видно в pg_stat_activity. Все наши соединения
    // (5+ collector'ов + executor) получают одну и ту же метку — Backend
    // умеет детектировать себя через `application_name == 'pgtop'`.
    // Игнорируем ошибку: даже если SET не пройдёт, остальное должно работать
    // — просто не сможем фильтровать self-rows.
    let _ = client.execute("SET application_name = 'pgtop'", &[]).await;

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

/// Запись в таблице блокировок (`pg_locks`).
///
/// `object` — это resolved-имя `schema.table` для `locktype = 'relation'`,
/// для остальных типов локов NULL (transactionid/virtualxid/advisory имеют
/// свои идентификаторы; для walking skeleton не показываем).
#[derive(Debug, Clone)]
pub struct Lock {
    pub pid: i32,
    pub locktype: String,
    pub mode: String,
    pub granted: bool,
    pub object: Option<String>,
}

/// SQL для locks view.
///
/// LEFT JOIN на `pg_class`/`pg_namespace` для resolve oid → schema.table.
/// `WHERE l.pid IS NOT NULL` отбрасывает prepared transactions (там pid NULL).
/// `ORDER BY granted, pid` — waiting (granted=false) поднимаются наверх,
/// что критично для мониторинга contention'а.
const LOCKS_QUERY: &str = "
SELECT
    l.pid,
    l.locktype,
    l.mode,
    l.granted,
    CASE
        WHEN l.locktype = 'relation' AND c.oid IS NOT NULL
        THEN n.nspname || '.' || c.relname
        ELSE NULL
    END AS object
FROM pg_locks l
LEFT JOIN pg_class c ON l.relation = c.oid
LEFT JOIN pg_namespace n ON c.relnamespace = n.oid
WHERE l.pid IS NOT NULL
  AND l.pid <> pg_backend_pid()
ORDER BY l.granted, l.pid
";

/// Снимает срез `pg_locks` (с LEFT JOIN-resolve relation oid в имя).
pub async fn fetch_locks(client: &Client) -> Result<Vec<Lock>, DbError> {
    let rows = client.query(LOCKS_QUERY, &[]).await?;
    Ok(rows.into_iter().map(row_to_lock).collect())
}

fn row_to_lock(row: Row) -> Lock {
    Lock {
        pid: row.get("pid"),
        locktype: row.get("locktype"),
        mode: row.get("mode"),
        granted: row.get("granted"),
        object: row.get("object"),
    }
}

/// Запись из `pg_stat_statements` — нормализованная статистика по уникальному
/// тексту запроса (с подставленными `$1`, `$2`).
#[derive(Debug, Clone)]
pub struct TopQuery {
    pub query: String,
    pub calls: i64,
    /// Cumulative time across all `calls`. Postgres хранит в миллисекундах.
    pub total_exec_time_ms: f64,
    /// Среднее время одного вызова в миллисекундах.
    pub mean_exec_time_ms: f64,
    /// Total rows returned/affected across all calls.
    pub rows: i64,
}

/// Snapshot Top Queries таба с тремя состояниями: Loading (до первого poll'а),
/// ExtensionMissing (расширение `pg_stat_statements` не установлено), Available.
///
/// Three-state enum вместо `Option<Vec<...>>` — потому что None в Option
/// не даёт различить «ещё не загрузили» и «недоступно». Первое — временное
/// и UI должен показать «загрузка»; второе — постоянное (до перезагрузки)
/// и UI должен показать инструкцию по установке.
#[derive(Debug, Clone)]
pub enum TopQueriesSnapshot {
    Loading,
    ExtensionMissing,
    Available(Vec<TopQuery>),
}

/// Имена колонок `pg_stat_statements` поменялись в PG13: было `total_time` /
/// `mean_time`, стало `total_exec_time` / `mean_exec_time` (плюс `*_plan_time`).
/// Мы используем PG16, ходим по новым именам.
const TOP_QUERIES_QUERY: &str = "
SELECT
    query,
    calls,
    total_exec_time AS total_ms,
    mean_exec_time  AS mean_ms,
    rows
FROM pg_stat_statements
ORDER BY total_exec_time DESC
LIMIT 50
";

/// Сначала проверяем наличие extension через `pg_extension`, потом запрашиваем
/// сами статистики. Без EXISTS-проверки запрос к `pg_stat_statements`
/// упал бы с «relation does not exist», и парсить ошибку для отличия
/// «нет extension» от «другая SQL-ошибка» ненадёжно.
pub async fn fetch_top_queries(client: &Client) -> Result<TopQueriesSnapshot, DbError> {
    let row = client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'pg_stat_statements')",
            &[],
        )
        .await?;
    let exists: bool = row.get(0);

    if !exists {
        return Ok(TopQueriesSnapshot::ExtensionMissing);
    }

    let rows = client.query(TOP_QUERIES_QUERY, &[]).await?;
    Ok(TopQueriesSnapshot::Available(
        rows.into_iter().map(row_to_top_query).collect(),
    ))
}

fn row_to_top_query(row: Row) -> TopQuery {
    TopQuery {
        query: row.get("query"),
        calls: row.get("calls"),
        total_exec_time_ms: row.get("total_ms"),
        mean_exec_time_ms: row.get("mean_ms"),
        rows: row.get("rows"),
    }
}

/// Запись из `pg_stat_replication` — клиент streaming-репликации.
///
/// `#[allow(dead_code)]`: SQL читает все полезные поля (`client_addr`,
/// `sent_lsn`), но текущий view показывает не всё. Появятся в Phase 4-style
/// detail view, если/когда будем делать.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Replica {
    pub pid: i32,
    pub application_name: Option<String>,
    pub client_addr: Option<String>,
    pub state: Option<String>,
    pub sync_state: Option<String>,
    /// Retreplay lag в секундах (`EXTRACT(EPOCH FROM replay_lag)`).
    /// `None` если реплика только подключилась (lag-fields ещё NULL).
    pub replay_lag_secs: Option<f64>,
    pub sent_lsn: Option<String>,
    pub replay_lsn: Option<String>,
}

/// `pg_lsn::text` даёт строковое представление LSN ("0/16B7E50") — без
/// зависимости от tokio-postgres-фичи под этот тип.
/// `EXTRACT(EPOCH FROM interval)::float8` превращает интервал в секунды
/// как float — компактно для отображения.
const REPLICATION_QUERY: &str = "
SELECT
    pid,
    application_name,
    client_addr::text                          AS client_addr,
    state,
    sync_state,
    EXTRACT(EPOCH FROM replay_lag)::float8     AS replay_lag_secs,
    sent_lsn::text                             AS sent_lsn,
    replay_lsn::text                           AS replay_lsn
FROM pg_stat_replication
ORDER BY pid
";

pub async fn fetch_replication(client: &Client) -> Result<Vec<Replica>, DbError> {
    let rows = client.query(REPLICATION_QUERY, &[]).await?;
    Ok(rows.into_iter().map(row_to_replica).collect())
}

fn row_to_replica(row: Row) -> Replica {
    Replica {
        pid: row.get("pid"),
        application_name: row.get("application_name"),
        client_addr: row.get("client_addr"),
        state: row.get("state"),
        sync_state: row.get("sync_state"),
        replay_lag_secs: row.get("replay_lag_secs"),
        sent_lsn: row.get("sent_lsn"),
        replay_lsn: row.get("replay_lsn"),
    }
}

/// Сводные метрики для шапки: TPS, активные соединения, cache hit ratio.
/// `tps` вычисляется в collector'е как дельта между двумя snapshot'ами, поэтому
/// первый снапшот отдаст 0.0 (нет previous для diff'а).
#[derive(Debug, Clone, Copy)]
pub struct Stats {
    pub tps: f64,
    pub active_connections: u32,
    pub cache_hit_pct: f64,
}

/// Сырые значения для подсчёта `Stats` — до diff'а на стороне collector'а.
#[derive(Debug, Clone, Copy)]
pub struct RawStats {
    /// Кумулятивное число транзакций (commit + rollback) по всем БД.
    pub xacts: i64,
    pub active_connections: u32,
    pub cache_hit_pct: f64,
}

/// Один query со тремя scalar-subqueries — экономит round-trip'ы.
///
/// Касты `::int8`/`::int4`/`::float8` обязательны: в Postgres `SUM(bigint)`
/// возвращает `numeric` (для overflow-safety), `COUNT(*)` — `bigint`,
/// и `100.0` — `numeric`. tokio-postgres без отдельного крейта (`rust-decimal`)
/// не умеет десериализовать numeric, поэтому форсим целевые типы в SQL.
///
/// `cache_hit_pct`: при отсутствии чтений (denom = 0) возвращаем 100% —
/// «всё в кэше», иначе DIVIDE BY ZERO. CASE-ветки кастятся в float8
/// независимо, чтобы итоговый column-тип был стабильно float8.
const STATS_QUERY: &str = "
SELECT
    (SELECT COALESCE(SUM(xact_commit + xact_rollback), 0)::int8
     FROM pg_stat_database) AS xacts,
    (SELECT COUNT(*)::int4
     FROM pg_stat_activity
     WHERE state = 'active') AS active_conns,
    (SELECT
        CASE
            WHEN SUM(blks_hit + blks_read) > 0
            THEN (100.0 * SUM(blks_hit) / SUM(blks_hit + blks_read))::float8
            ELSE 100.0::float8
        END
     FROM pg_stat_database) AS cache_hit_pct
";

pub async fn fetch_raw_stats(client: &Client) -> Result<RawStats, DbError> {
    let row = client.query_one(STATS_QUERY, &[]).await?;
    Ok(RawStats {
        xacts: row.get("xacts"),
        active_connections: row.get::<_, i32>("active_conns") as u32,
        cache_hit_pct: row.get("cache_hit_pct"),
    })
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
