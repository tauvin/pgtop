//! Сборщик `pg_locks`: опрашивает раз в секунду, публикует через
//! shared `mpsc::UnboundedSender<UpdateMessage>` (Phase 8 Block B).

use std::time::Duration;

use tokio::{
    sync::mpsc,
    time::{MissedTickBehavior, interval},
};
use tokio_postgres::Client;
use tokio_util::sync::CancellationToken;

use crate::db;
use crate::messages::UpdateMessage;

pub async fn run_locks_collector(
    client: Client,
    tx: mpsc::UnboundedSender<UpdateMessage>,
    conn_idx: usize,
    cancel: CancellationToken,
    poll_interval: Duration,
) {
    let mut ticker = interval(poll_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            _ = ticker.tick() => {}
        }

        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            r = db::fetch_locks(&client) => r,
        };

        if let Ok(snapshot) = result
            && tx
                .send(UpdateMessage::Locks { conn_idx, snapshot })
                .is_err()
        {
            break;
        }
    }
}
