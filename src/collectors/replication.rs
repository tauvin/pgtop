//! Сборщик `pg_stat_replication`: 5-секундный интервал — реплика-state
//! меняется медленно. У большинства настроек таблица будет пустая (нет
//! активных реплик), поэтому view нужно уметь показывать empty state.

use std::time::Duration;

use tokio::{
    sync::watch,
    time::{MissedTickBehavior, interval},
};
use tokio_postgres::Client;
use tokio_util::sync::CancellationToken;

use crate::db::{self, Replica};

pub async fn run_replication_collector(
    client: Client,
    tx: watch::Sender<Vec<Replica>>,
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
            r = db::fetch_replication(&client) => r,
        };

        match result {
            Ok(replicas) => {
                if tx.send(replicas).is_err() {
                    break;
                }
            }
            Err(_) => continue,
        }
    }
}
