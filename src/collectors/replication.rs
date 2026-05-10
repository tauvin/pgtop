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

const POLL_INTERVAL: Duration = Duration::from_secs(5);

pub async fn run_replication_collector(
    client: Client,
    tx: watch::Sender<Vec<Replica>>,
    cancel: CancellationToken,
) {
    let mut ticker = interval(POLL_INTERVAL);
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
