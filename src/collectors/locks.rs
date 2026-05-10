//! `pg_locks` collector. Reconnects silently on connection loss.

use std::time::Duration;

use tokio::{
    sync::mpsc,
    time::{MissedTickBehavior, interval},
};
use tokio_util::sync::CancellationToken;

use crate::db;
use crate::messages::UpdateMessage;

pub async fn run_locks_collector(
    dsn: String,
    tx: mpsc::UnboundedSender<UpdateMessage>,
    conn_idx: usize,
    cancel: CancellationToken,
    poll_interval: Duration,
) {
    'outer: loop {
        let client = match db::connect_with_backoff(&dsn, &cancel, |_| {}).await {
            Some(c) => c,
            None => return,
        };

        let mut ticker = interval(poll_interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return,
                _ = ticker.tick() => {}
            }

            if client.is_closed() {
                continue 'outer;
            }

            let result = tokio::select! {
                biased;
                _ = cancel.cancelled() => return,
                r = db::fetch_locks(&client) => r,
            };

            match result {
                Ok(snapshot) => {
                    if tx
                        .send(UpdateMessage::Locks { conn_idx, snapshot })
                        .is_err()
                    {
                        return;
                    }
                }
                Err(_) if client.is_closed() => continue 'outer,
                Err(_) => {}
            }
        }
    }
}
