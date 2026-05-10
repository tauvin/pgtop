//! Сборщик `pg_stat_statements`: 10-секундный интервал (статистика
//! агрегатная, чаще опрашивать смысла нет). Отличается от activity/locks
//! тем, что extension может быть не установлен — fetch возвращает
//! `TopQueriesSnapshot` с тремя состояниями (см. db.rs), а не голый Vec.

use std::time::Duration;

use tokio::{
    sync::watch,
    time::{MissedTickBehavior, interval},
};
use tokio_postgres::Client;
use tokio_util::sync::CancellationToken;

use crate::db::{self, TopQueriesSnapshot};

pub async fn run_top_queries_collector(
    client: Client,
    tx: watch::Sender<TopQueriesSnapshot>,
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
            r = db::fetch_top_queries(&client) => r,
        };

        match result {
            Ok(snapshot) => {
                if tx.send(snapshot).is_err() {
                    break;
                }
            }
            Err(_) => continue,
        }
    }
}
