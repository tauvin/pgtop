//! Ad-hoc EXPLAIN runner: spawns a one-shot connection, runs `EXPLAIN`
//! against the requested query, and publishes the plan text via the
//! shared update channel.
//!
//! Safety against runaway plans:
//! - `SET statement_timeout = '5s'` is set before the EXPLAIN runs, so a
//!   pathological planner cannot wedge a connection indefinitely.
//! - When the caller cancels mid-query (popup closed, conn switched, app
//!   shutdown), we issue a Postgres-protocol `CancelRequest` on a fresh
//!   plaintext connection via `Client::cancel_token`. Best-effort —
//!   managed providers requiring TLS for the cancel sub-protocol will
//!   reject, in which case `statement_timeout` still bounds the wedge.
//!
//! No `EXPLAIN ANALYZE` — that would actually execute the query, which is
//! unsafe unattended. Plain EXPLAIN works for any DML plan and is
//! read-only.
//!
//! Threat model for the EXPLAIN string itself: `query` is sourced from
//! `pg_stat_activity.query` on the monitored server, so it is whatever a
//! session there happens to be running. tokio-postgres's simple-protocol
//! `query()` already rejects multi-statement strings, so a malicious
//! `; DROP TABLE` suffix won't execute — but EXPLAIN-ing a statement that
//! ends with `;` or contains a `;` outside a literal is a malformed
//! request and produces a confusing error. We strip the trailing `;`
//! (idiomatic in psql) and reject anything with an inner `;` since it's
//! either truncated mid-statement or otherwise not a plannable single
//! statement.

use tokio::sync::mpsc;
use tokio_postgres::NoTls;
use tokio_util::sync::CancellationToken;

use crate::db;
use crate::messages::UpdateMessage;

pub async fn run_explain(
    dsn: String,
    query: String,
    conn_idx: usize,
    tx: mpsc::UnboundedSender<UpdateMessage>,
    cancel: CancellationToken,
) {
    let plan = explain(&dsn, &query, &cancel).await;
    let _ = tx.send(UpdateMessage::ExplainResult { conn_idx, plan });
}

async fn explain(dsn: &str, query: &str, cancel: &CancellationToken) -> Result<String, String> {
    let cleaned = sanitize_for_explain(query)?;

    let client = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err("cancelled".into()),
        r = db::connect(dsn) => r.map_err(|e| e.to_string())?,
    };

    if let Err(e) = client.execute("SET statement_timeout = '5s'", &[]).await {
        return Err(format!("could not set statement_timeout: {e}"));
    }

    let pg_cancel = client.cancel_token();
    let sql = format!("EXPLAIN {cleaned}");
    let rows = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            let _ = pg_cancel.cancel_query(NoTls).await;
            return Err("cancelled".into());
        }
        r = client.query(&sql, &[]) => r.map_err(|e| e.to_string())?,
    };

    let lines: Vec<String> = rows.into_iter().map(|r| r.get::<_, String>(0)).collect();
    Ok(lines.join("\n"))
}

/// Trim trailing `;` and reject queries that still contain `;` inside.
/// Postgres syntactic context (literals, comments) is not parsed — any
/// inner `;` is treated as a structural red flag, which is conservative
/// but correct for the common cases (multi-statement, truncated query).
fn sanitize_for_explain(query: &str) -> Result<&str, String> {
    let trimmed = query.trim().trim_end_matches(';').trim_end();
    if trimmed.is_empty() {
        return Err("query is empty".into());
    }
    if trimmed.contains(';') {
        return Err("cannot EXPLAIN a multi-statement or truncated query".into());
    }
    Ok(trimmed)
}

#[cfg(test)]
mod tests {
    use super::sanitize_for_explain;

    #[test]
    fn strips_trailing_semicolon() {
        assert_eq!(sanitize_for_explain("SELECT 1;").unwrap(), "SELECT 1");
        assert_eq!(sanitize_for_explain("SELECT 1 ; ").unwrap(), "SELECT 1");
    }

    #[test]
    fn rejects_inner_semicolon() {
        assert!(sanitize_for_explain("SELECT 1; DROP TABLE x").is_err());
    }

    #[test]
    fn rejects_empty() {
        assert!(sanitize_for_explain("").is_err());
        assert!(sanitize_for_explain("  ;  ").is_err());
    }

    #[test]
    fn passes_clean_query() {
        assert_eq!(
            sanitize_for_explain("SELECT * FROM t WHERE x = 1").unwrap(),
            "SELECT * FROM t WHERE x = 1"
        );
    }
}
