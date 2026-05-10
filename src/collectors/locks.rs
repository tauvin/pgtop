//! Сборщик `pg_locks`: опрашивает раз в секунду, публикует
//! `Vec<Lock>` через `watch::channel`. Структурно идентичен activity-collector'у —
//! отличие только в SQL и snapshot-типе.

use std::time::Duration;

use tokio::{
    sync::watch,
    time::{MissedTickBehavior, interval},
};
use tokio_postgres::Client;
use tokio_util::sync::CancellationToken;

use crate::db::{self, Lock};

const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Контракт идентичен `run_activity_collector`. См. `collectors/activity.rs`
/// за подробностями про `biased;`-cancel-семантику и сигнал «UI ушёл» через
/// `tx.send().is_err()`.
pub async fn run_locks_collector(
    client: Client,
    tx: watch::Sender<Vec<Lock>>,
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
            r = db::fetch_locks(&client) => r,
        };

        match result {
            Ok(locks) => {
                if tx.send(locks).is_err() {
                    break;
                }
            }
            Err(_) => continue,
        }
    }
}
