//! Low-level Postgres access: connection setup and `pg_stat_*` queries.
//! Higher layers (collectors, UI) consume the typed structs defined here.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rustls::ClientConfig;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::ring;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, RootCertStore, SignatureScheme};
use thiserror::Error;
use tokio_postgres::{Client, Config, Row};
use tokio_postgres_rustls::MakeRustlsConnect;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Error)]
pub enum DbError {
    #[error("postgres error: {0}")]
    Postgres(#[from] tokio_postgres::Error),
    #[error("invalid dsn: {0}")]
    Dsn(String),
}

/// Subset of `pg_stat_activity` columns consumed by pgtop. Nullable columns
/// in Postgres are mapped to `Option<T>`; reading a NULL into a non-Option
/// would panic.
#[derive(Debug, Clone)]
pub struct Backend {
    pub pid: i32,
    pub datname: Option<String>,
    pub usename: Option<String>,
    pub application_name: Option<String>,
    /// Cast to text in SQL to avoid the inet binary format dependency.
    pub client_addr: Option<String>,
    /// Documented as NOT NULL but observed NULL in the wild for some
    /// background workers during init.
    pub backend_start: Option<DateTime<Utc>>,
    pub xact_start: Option<DateTime<Utc>>,
    pub query_start: Option<DateTime<Utc>>,
    pub state_change: Option<DateTime<Utc>>,
    pub wait_event_type: Option<String>,
    pub wait_event: Option<String>,
    pub state: Option<String>,
    /// Cast to text to avoid the xid type dependency.
    pub backend_xid: Option<String>,
    pub backend_xmin: Option<String>,
    pub query: Option<String>,
    pub backend_type: Option<String>,
}

impl Backend {
    /// True if this row is one of pgtop's own connections, identified by
    /// `application_name = 'pgtop'`.
    pub fn is_self(&self) -> bool {
        self.application_name.as_deref() == Some("pgtop")
    }
}

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

/// Connect to Postgres with TLS support driven by the DSN's `sslmode`,
/// spawn the connection driver, and tag the session with
/// `application_name = 'pgtop'`.
///
/// Honours all five libpq sslmode values: `disable`, `prefer` (default),
/// `require`, `verify-ca`, `verify-full`. The verify-* modes are
/// pre-processed: tokio-postgres only knows the first three, so we
/// translate verify-{ca,full} to require and turn on cert verification
/// against the Mozilla root store (`webpki-roots`).
pub async fn connect(dsn: &str) -> Result<Client, DbError> {
    let (cleaned, verify_certs) = rewrite_verify_sslmode(dsn);
    let config: Config = cleaned
        .parse()
        .map_err(|e: tokio_postgres::Error| DbError::Dsn(e.to_string()))?;
    let connector = make_connector(verify_certs);
    let (client, connection) = config.connect(connector).await?;

    tokio::spawn(async move {
        if let Err(e) = connection.await {
            tracing::error!("postgres connection driver error: {e}");
        }
    });

    let _ = client.execute("SET application_name = 'pgtop'", &[]).await;

    Ok(client)
}

/// tokio-postgres 0.7 rejects `sslmode=verify-{ca,full}` at parse time. We
/// translate them to `sslmode=require` and let the caller request cert
/// verification through the connector instead.
fn rewrite_verify_sslmode(dsn: &str) -> (String, bool) {
    if dsn.contains("sslmode=verify-full") {
        (dsn.replace("sslmode=verify-full", "sslmode=require"), true)
    } else if dsn.contains("sslmode=verify-ca") {
        (dsn.replace("sslmode=verify-ca", "sslmode=require"), true)
    } else {
        (dsn.to_string(), false)
    }
}

fn make_connector(verify_certs: bool) -> MakeRustlsConnect {
    let config = if verify_certs {
        verifying_tls_config()
    } else {
        no_verify_tls_config()
    };
    MakeRustlsConnect::new(config)
}

fn verifying_tls_config() -> ClientConfig {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("rustls supports default protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth()
}

fn no_verify_tls_config() -> ClientConfig {
    ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("rustls supports default protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerify))
        .with_no_client_auth()
}

#[derive(Debug)]
struct NoVerify;

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::ED25519,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
        ]
    }
}

/// Connect with exponential backoff (500ms → 30s cap). `on_attempt` is
/// invoked with each 1-based attempt number before connecting.
///
/// Returns `None` when the cancellation token fires — the caller should
/// `return` for graceful shutdown.
pub async fn connect_with_backoff(
    dsn: &str,
    cancel: &CancellationToken,
    mut on_attempt: impl FnMut(u32),
) -> Option<Client> {
    let mut delay = Duration::from_millis(500);
    let max_delay = Duration::from_secs(30);
    let mut attempt: u32 = 1;

    loop {
        on_attempt(attempt);

        let connect_result = tokio::select! {
            biased;
            _ = cancel.cancelled() => return None,
            r = connect(dsn) => r,
        };

        match connect_result {
            Ok(client) => return Some(client),
            Err(e) => {
                tracing::warn!(attempt, error = %e, "postgres connect failed, will retry");
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return None,
                    _ = tokio::time::sleep(delay) => {}
                }
                delay = (delay * 2).min(max_delay);
                attempt = attempt.saturating_add(1);
            }
        }
    }
}

/// Snapshot `pg_stat_activity` into a `Vec<Backend>`.
pub async fn fetch_backends(client: &Client) -> Result<Vec<Backend>, DbError> {
    let rows = client.query(ACTIVITY_QUERY, &[]).await?;
    Ok(rows.into_iter().map(row_to_backend).collect())
}

/// One row from `pg_locks`. `object` resolves to `schema.table` for
/// `locktype = 'relation'` and is `None` otherwise.
#[derive(Debug, Clone)]
pub struct Lock {
    pub pid: i32,
    pub locktype: String,
    pub mode: String,
    pub granted: bool,
    pub object: Option<String>,
}

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

/// Snapshot `pg_locks`, resolving relation OIDs to `schema.table` names.
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

/// One row from `pg_stat_statements` — normalised statistics per unique
/// query text.
#[derive(Debug, Clone)]
pub struct TopQuery {
    pub query: String,
    pub calls: i64,
    /// Cumulative time across all calls, milliseconds.
    pub total_exec_time_ms: f64,
    /// Mean time per call, milliseconds.
    pub mean_exec_time_ms: f64,
    /// Total rows returned/affected across all calls.
    pub rows: i64,
}

/// Top Queries snapshot with three states: not yet polled, extension
/// missing, or available data.
#[derive(Debug, Clone)]
pub enum TopQueriesSnapshot {
    Loading,
    ExtensionMissing,
    Available(Vec<TopQuery>),
}

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

/// Check that `pg_stat_statements` is installed and return the top queries.
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

/// One row from `pg_stat_replication` — a streaming replication client.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Replica {
    pub pid: i32,
    pub application_name: Option<String>,
    pub client_addr: Option<String>,
    pub state: Option<String>,
    pub sync_state: Option<String>,
    /// Replay lag in seconds. `None` for newly-connected replicas.
    pub replay_lag_secs: Option<f64>,
    pub sent_lsn: Option<String>,
    pub replay_lsn: Option<String>,
}

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

/// Header summary metrics: TPS, active connections, cache hit ratio.
/// `tps` is computed by the collector as a delta between snapshots.
#[derive(Debug, Clone, Copy)]
pub struct Stats {
    pub tps: f64,
    pub active_connections: u32,
    pub cache_hit_pct: f64,
}

/// Raw values used by the collector to derive `Stats`.
#[derive(Debug, Clone, Copy)]
pub struct RawStats {
    /// Cumulative `xact_commit + xact_rollback` across all databases.
    pub xacts: i64,
    pub active_connections: u32,
    pub cache_hit_pct: f64,
}

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

/// One row from `pg_stat_database`. Cumulative counters (commits, rollbacks,
/// blocks, temp_bytes, deadlocks) reflect the value at fetch time, not the
/// rate over the polling interval.
#[derive(Debug, Clone)]
pub struct DatabaseStat {
    pub datname: String,
    pub numbackends: i32,
    pub xact_commit: i64,
    pub xact_rollback: i64,
    pub blks_hit: i64,
    pub blks_read: i64,
    pub temp_bytes: i64,
    pub deadlocks: i64,
}

impl DatabaseStat {
    /// Cache hit ratio as a percentage. `100.0` for databases with zero
    /// reads — better default than NaN for an empty/idle database.
    pub fn cache_hit_pct(&self) -> f64 {
        let total = self.blks_hit + self.blks_read;
        if total > 0 {
            100.0 * self.blks_hit as f64 / total as f64
        } else {
            100.0
        }
    }
}

const DATABASES_QUERY: &str = "
SELECT
    datname,
    numbackends,
    xact_commit,
    xact_rollback,
    blks_hit,
    blks_read,
    temp_bytes,
    deadlocks
FROM pg_stat_database
WHERE datname IS NOT NULL
ORDER BY xact_commit + xact_rollback DESC
";

/// Snapshot `pg_stat_database`. The `datname IS NOT NULL` filter skips the
/// global stats row (shared catalogues / no specific database).
pub async fn fetch_database_stats(client: &Client) -> Result<Vec<DatabaseStat>, DbError> {
    let rows = client.query(DATABASES_QUERY, &[]).await?;
    Ok(rows
        .into_iter()
        .map(|row| DatabaseStat {
            datname: row.get("datname"),
            numbackends: row.get("numbackends"),
            xact_commit: row.get("xact_commit"),
            xact_rollback: row.get("xact_rollback"),
            blks_hit: row.get("blks_hit"),
            blks_read: row.get("blks_read"),
            temp_bytes: row.get("temp_bytes"),
            deadlocks: row.get("deadlocks"),
        })
        .collect())
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_verify_sslmode_passes_through_other_modes() {
        assert_eq!(
            rewrite_verify_sslmode("postgres://h/d?sslmode=disable"),
            ("postgres://h/d?sslmode=disable".to_string(), false)
        );
        assert_eq!(
            rewrite_verify_sslmode("postgres://h/d?sslmode=require"),
            ("postgres://h/d?sslmode=require".to_string(), false)
        );
        assert_eq!(
            rewrite_verify_sslmode("host=h dbname=d"),
            ("host=h dbname=d".to_string(), false)
        );
    }

    #[test]
    fn rewrite_verify_sslmode_translates_verify_full() {
        let (cleaned, verify) = rewrite_verify_sslmode("postgres://h/d?sslmode=verify-full");
        assert_eq!(cleaned, "postgres://h/d?sslmode=require");
        assert!(verify);
    }

    #[test]
    fn rewrite_verify_sslmode_translates_verify_ca() {
        let (cleaned, verify) = rewrite_verify_sslmode("host=h sslmode=verify-ca dbname=d");
        assert_eq!(cleaned, "host=h sslmode=require dbname=d");
        assert!(verify);
    }
}
