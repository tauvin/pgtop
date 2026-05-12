//! Low-level Postgres access: connection setup and `pg_stat_*` queries.
//! Higher layers (collectors, UI) consume the typed structs defined here.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use regex::Regex;
use rustls::ClientConfig;
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::ring;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, RootCertStore, SignatureScheme};
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
/// translate verify-{ca,full} to `require` and pick the correct cert
/// verifier (chain-only vs chain+hostname) at the connector level.
pub async fn connect(dsn: &str) -> Result<Client, DbError> {
    let (cleaned, verify_mode) = rewrite_verify_sslmode(dsn);
    let config: Config = cleaned
        .parse()
        .map_err(|e: tokio_postgres::Error| DbError::Dsn(e.to_string()))?;
    let connector = make_connector(verify_mode);
    let (client, connection) = config.connect(connector).await?;

    tokio::spawn(async move {
        if let Err(e) = connection.await {
            tracing::error!("postgres connection driver error: {e}");
        }
    });

    // Failure here means our own backends won't be tagged as 'pgtop' and
    // will appear in the activity table as ordinary connections — surface
    // it but don't abort the connect, the client is otherwise usable.
    if let Err(e) = client.execute("SET application_name = 'pgtop'", &[]).await {
        tracing::warn!(error = %e, "failed to set application_name on new connection");
    }

    Ok(client)
}

/// Cert-verification level requested by the user. Mirrors libpq's three
/// distinct security levels for `sslmode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyMode {
    /// `disable` / `prefer` / `require` — no chain or hostname check.
    None,
    /// `verify-ca` — chain validates against trusted roots; hostname is
    /// not required to match. Useful for self-signed setups where the CA
    /// is trusted but certs have a different CN than the host you connect
    /// to.
    ChainOnly,
    /// `verify-full` — chain plus hostname match against the DSN host.
    Full,
}

/// tokio-postgres 0.7 rejects `sslmode=verify-{ca,full}` at parse time. We
/// translate them to `sslmode=require` and let the caller pick the
/// matching verifier through the connector.
///
/// A naïve `dsn.contains("sslmode=verify-...")` would also match the
/// literal text occurring elsewhere in the DSN (e.g. inside a password
/// or `application_name`), which could either downgrade verification or
/// upgrade it incorrectly. A word-boundaried regex constrains the match
/// to a real key/value pair. Full DSN parsing remains deferred.
fn rewrite_verify_sslmode(dsn: &str) -> (String, VerifyMode) {
    use std::sync::LazyLock;

    static FULL_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\bsslmode=verify-full\b").unwrap());
    static CA_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bsslmode=verify-ca\b").unwrap());

    if FULL_RE.is_match(dsn) {
        (
            FULL_RE.replace_all(dsn, "sslmode=require").into_owned(),
            VerifyMode::Full,
        )
    } else if CA_RE.is_match(dsn) {
        (
            CA_RE.replace_all(dsn, "sslmode=require").into_owned(),
            VerifyMode::ChainOnly,
        )
    } else {
        (dsn.to_string(), VerifyMode::None)
    }
}

fn make_connector(mode: VerifyMode) -> MakeRustlsConnect {
    let config = match mode {
        VerifyMode::None => no_verify_tls_config(),
        VerifyMode::ChainOnly => chain_only_tls_config(),
        VerifyMode::Full => verifying_tls_config(),
    };
    MakeRustlsConnect::new(config)
}

fn webpki_roots_store() -> Arc<RootCertStore> {
    // The bundled Mozilla root store is identical for the lifetime of the
    // process — cache it after the first build so every collector's first
    // connect doesn't re-walk webpki_roots::TLS_SERVER_ROOTS.
    static STORE: std::sync::OnceLock<Arc<RootCertStore>> = std::sync::OnceLock::new();
    STORE
        .get_or_init(|| {
            let mut roots = RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            Arc::new(roots)
        })
        .clone()
}

fn verifying_tls_config() -> ClientConfig {
    ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("rustls supports default protocol versions")
        .with_root_certificates((*webpki_roots_store()).clone())
        .with_no_client_auth()
}

fn chain_only_tls_config() -> ClientConfig {
    let inner = WebPkiServerVerifier::builder_with_provider(
        webpki_roots_store(),
        Arc::new(ring::default_provider()),
    )
    .build()
    .expect("webpki verifier builds with bundled roots");
    ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("rustls supports default protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(ChainOnlyVerifier { inner }))
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

/// `verify-ca` semantics: delegate to the standard webpki verifier for
/// chain validation, then ignore hostname-mismatch errors. Signature
/// verification still runs through the inner verifier.
#[derive(Debug)]
struct ChainOnlyVerifier {
    inner: Arc<WebPkiServerVerifier>,
}

impl ServerCertVerifier for ChainOnlyVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        match self.inner.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        ) {
            Err(rustls::Error::InvalidCertificate(CertificateError::NotValidForName))
            | Err(rustls::Error::InvalidCertificate(CertificateError::NotValidForNameContext {
                ..
            })) => Ok(ServerCertVerified::assertion()),
            other => other,
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
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
/// The view only renders a subset of fields (`application_name`,
/// `sync_state`, `replay_lag_secs`); the rest are still selected and
/// kept on the struct so adding columns to the view doesn't require a
/// schema change to the SQL.
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
    let raw_active: i32 = row.get("active_conns");
    Ok(RawStats {
        xacts: row.get("xacts"),
        // COUNT(*) is non-negative by definition, but a defensive
        // try_into avoids a silent sign-flip if the query ever changes.
        active_connections: u32::try_from(raw_active).unwrap_or(0),
        cache_hit_pct: row.get("cache_hit_pct"),
    })
}

/// One row from `pg_stat_database`. Cumulative counters (commits, rollbacks,
/// blocks, temp_bytes, deadlocks) reflect the value at fetch time, not the
/// rate over the polling interval. `tps` is derived in the collector from
/// the delta between consecutive snapshots; `None` until two snapshots
/// have been seen for this database.
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
    pub tps: Option<f64>,
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

/// One row from `pg_stat_user_tables`. `last_vacuum` and `last_analyze`
/// hold the most recent of the manual / autovacuum timestamps.
#[derive(Debug, Clone)]
pub struct TableStat {
    pub schemaname: String,
    pub relname: String,
    pub n_live_tup: i64,
    pub n_dead_tup: i64,
    pub last_vacuum: Option<DateTime<Utc>>,
    pub last_analyze: Option<DateTime<Utc>>,
    pub seq_scan: i64,
    pub idx_scan: i64,
}

impl TableStat {
    /// Dead tuples as a fraction of total tuples, in percent. Returns
    /// `None` for an empty relation (no live tuples), where the ratio
    /// would be undefined / misleading.
    pub fn dead_pct(&self) -> Option<f64> {
        let total = self.n_live_tup + self.n_dead_tup;
        if total > 0 {
            Some(100.0 * self.n_dead_tup as f64 / total as f64)
        } else {
            None
        }
    }
}

const TABLES_QUERY: &str = "
SELECT
    schemaname,
    relname,
    n_live_tup,
    n_dead_tup,
    GREATEST(last_vacuum, last_autovacuum) AS last_vacuum,
    GREATEST(last_analyze, last_autoanalyze) AS last_analyze,
    seq_scan,
    idx_scan
FROM pg_stat_user_tables
ORDER BY n_dead_tup DESC NULLS LAST, n_live_tup DESC
LIMIT 50
";

/// Snapshot `pg_stat_user_tables`, top 50 tables by dead-tuple count.
pub async fn fetch_table_stats(client: &Client) -> Result<Vec<TableStat>, DbError> {
    let rows = client.query(TABLES_QUERY, &[]).await?;
    Ok(rows
        .into_iter()
        .map(|row| TableStat {
            schemaname: row.get("schemaname"),
            relname: row.get("relname"),
            n_live_tup: row.get("n_live_tup"),
            n_dead_tup: row.get("n_dead_tup"),
            last_vacuum: row.get("last_vacuum"),
            last_analyze: row.get("last_analyze"),
            seq_scan: row.get::<_, Option<i64>>("seq_scan").unwrap_or(0),
            idx_scan: row.get::<_, Option<i64>>("idx_scan").unwrap_or(0),
        })
        .collect())
}

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
            tps: None,
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
            (
                "postgres://h/d?sslmode=disable".to_string(),
                VerifyMode::None
            )
        );
        assert_eq!(
            rewrite_verify_sslmode("postgres://h/d?sslmode=require"),
            (
                "postgres://h/d?sslmode=require".to_string(),
                VerifyMode::None
            )
        );
        assert_eq!(
            rewrite_verify_sslmode("host=h dbname=d"),
            ("host=h dbname=d".to_string(), VerifyMode::None)
        );
    }

    #[test]
    fn rewrite_verify_sslmode_translates_verify_full() {
        let (cleaned, mode) = rewrite_verify_sslmode("postgres://h/d?sslmode=verify-full");
        assert_eq!(cleaned, "postgres://h/d?sslmode=require");
        assert_eq!(mode, VerifyMode::Full);
    }

    #[test]
    fn rewrite_verify_sslmode_translates_verify_ca() {
        let (cleaned, mode) = rewrite_verify_sslmode("host=h sslmode=verify-ca dbname=d");
        assert_eq!(cleaned, "host=h sslmode=require dbname=d");
        assert_eq!(mode, VerifyMode::ChainOnly);
    }

    #[test]
    fn rewrite_verify_sslmode_picks_full_when_both_textually_present() {
        // verify-full takes precedence over verify-ca to be on the safer side
        // when a DSN is malformed (libpq itself rejects this case).
        let (cleaned, mode) =
            rewrite_verify_sslmode("postgres://h/d?sslmode=verify-full&backup=verify-ca");
        assert!(cleaned.contains("sslmode=require"));
        assert_eq!(mode, VerifyMode::Full);
    }

    #[test]
    fn rewrite_verify_sslmode_ignores_password_substrings() {
        // Password contains the literal 'verify-ca' as a substring. Naïve
        // string-contains would have downgraded the verification level.
        let dsn = "postgres://u:my_verify-ca_pw@h/d?sslmode=require";
        let (cleaned, mode) = rewrite_verify_sslmode(dsn);
        assert_eq!(cleaned, dsn);
        assert_eq!(mode, VerifyMode::None);
    }
}
