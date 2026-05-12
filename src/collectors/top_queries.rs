//! `pg_stat_statements` collector. Reconnects silently on connection loss.

use std::time::Duration;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::run_simple_collector;
use crate::db;
use crate::messages::UpdateMessage;

pub async fn run_top_queries_collector(
    dsn: String,
    tx: mpsc::Sender<UpdateMessage>,
    conn_idx: usize,
    cancel: CancellationToken,
    poll_interval: Duration,
) {
    run_simple_collector(
        "top_queries",
        dsn,
        tx,
        conn_idx,
        cancel,
        poll_interval,
        db::fetch_top_queries,
        |conn_idx, snapshot| UpdateMessage::TopQueries { conn_idx, snapshot },
    )
    .await;
}
