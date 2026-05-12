//! `pg_stat_user_tables` collector. Silent reconnect, default 10s interval —
//! bloat counters change slowly relative to activity polling.

use std::time::Duration;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::run_simple_collector;
use crate::db;
use crate::messages::UpdateMessage;

pub async fn run_tables_collector(
    dsn: String,
    tx: mpsc::Sender<UpdateMessage>,
    conn_idx: usize,
    cancel: CancellationToken,
    poll_interval: Duration,
) {
    run_simple_collector(
        "tables",
        dsn,
        tx,
        conn_idx,
        cancel,
        poll_interval,
        db::fetch_table_stats,
        |conn_idx, snapshot| UpdateMessage::Tables { conn_idx, snapshot },
    )
    .await;
}
