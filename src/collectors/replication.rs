//! `pg_stat_replication` collector. Reconnects silently on connection loss.

use std::time::Duration;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::run_simple_collector;
use crate::db;
use crate::messages::UpdateMessage;

pub async fn run_replication_collector(
    dsn: String,
    tx: mpsc::UnboundedSender<UpdateMessage>,
    conn_idx: usize,
    cancel: CancellationToken,
    poll_interval: Duration,
) {
    run_simple_collector(
        "replication",
        dsn,
        tx,
        conn_idx,
        cancel,
        poll_interval,
        db::fetch_replication,
        |conn_idx, snapshot| UpdateMessage::Replication { conn_idx, snapshot },
    )
    .await;
}
