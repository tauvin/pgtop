//! `pg_stat_activity` collector. Owns its connection lifecycle and reconnects
//! with exponential backoff. The only collector that publishes
//! `ConnectionStatus` updates; others reconnect silently.

use std::time::Duration;

use tokio::{
    sync::mpsc,
    time::{MissedTickBehavior, interval},
};
use tokio_util::sync::CancellationToken;

use crate::app::ConnectionStatus;
use crate::db;
use crate::messages::UpdateMessage;

pub async fn run_activity_collector(
    dsn: String,
    tx: mpsc::UnboundedSender<UpdateMessage>,
    conn_idx: usize,
    cancel: CancellationToken,
    poll_interval: Duration,
) {
    'outer: loop {
        let tx_for_status = tx.clone();
        let client = match db::connect_with_backoff(&dsn, &cancel, move |attempt| {
            let _ = tx_for_status.send(UpdateMessage::Status {
                conn_idx,
                status: ConnectionStatus::Connecting { attempt },
            });
        })
        .await
        {
            Some(c) => c,
            None => return,
        };

        if tx
            .send(UpdateMessage::Status {
                conn_idx,
                status: ConnectionStatus::Connected,
            })
            .is_err()
        {
            return;
        }

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
                r = db::fetch_backends(&client) => r,
            };

            match result {
                Ok(snapshot) => {
                    if tx
                        .send(UpdateMessage::Activity { conn_idx, snapshot })
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
