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
    let client = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err("cancelled".into()),
        r = db::connect(dsn) => r.map_err(|e| e.to_string())?,
    };

    if let Err(e) = client.execute("SET statement_timeout = '5s'", &[]).await {
        return Err(format!("could not set statement_timeout: {e}"));
    }

    let pg_cancel = client.cancel_token();
    let sql = format!("EXPLAIN {query}");
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
