//! Сборщик `pg_stat_activity`: опрашивает раз в секунду, публикует
//! `Vec<Backend>` через shared `mpsc::UnboundedSender<UpdateMessage>`.

use std::time::Duration;

use tokio::{
    sync::mpsc,
    time::{MissedTickBehavior, interval},
};
use tokio_postgres::Client;
use tokio_util::sync::CancellationToken;

use crate::db;
use crate::messages::UpdateMessage;

/// Запустить сборщик `pg_stat_activity` в текущей tokio-task. `conn_idx`
/// идентифицирует целевое соединение в App.connections — wraps в
/// `UpdateMessage::Activity { conn_idx, snapshot }`.
pub async fn run_activity_collector(
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
            r = db::fetch_backends(&client) => r,
        };

        if let Ok(snapshot) = result
            && tx
                .send(UpdateMessage::Activity { conn_idx, snapshot })
                .is_err()
        {
            break; // UI ушёл — нам тоже пора
        }
    }
}
