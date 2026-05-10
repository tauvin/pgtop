//! Ad-hoc EXPLAIN runner: spawns a one-shot connection, runs `EXPLAIN`
//! against the requested query, and publishes the plan text via the
//! shared update channel.
//!
//! No `ANALYZE` — that would actually execute the query, which is unsafe
//! to do unattended against production. Plain EXPLAIN works for any
//! `SELECT/INSERT/UPDATE/DELETE` plan and is read-only.

use tokio::sync::mpsc;
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

    let sql = format!("EXPLAIN {query}");
    let rows = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err("cancelled".into()),
        r = client.query(&sql, &[]) => r.map_err(|e| e.to_string())?,
    };

    let lines: Vec<String> = rows.into_iter().map(|r| r.get::<_, String>(0)).collect();
    Ok(lines.join("\n"))
}
