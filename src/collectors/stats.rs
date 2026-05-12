//! Header summary metrics collector: TPS, active connections, cache hit %.
//! Stateful — keeps the previous snapshot to compute the TPS delta. Resets
//! its previous-state on reconnect.

use std::time::{Duration, Instant};

use tokio::{
    sync::mpsc,
    time::{MissedTickBehavior, interval},
};
use tokio_util::sync::CancellationToken;

use super::try_publish;
use crate::db::{self, Stats};
use crate::messages::UpdateMessage;

pub async fn run_stats_collector(
    dsn: String,
    tx: mpsc::Sender<UpdateMessage>,
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

        let mut prev: Option<(i64, Instant)> = None;

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
                r = db::fetch_raw_stats(&client) => r,
            };

            let raw = match result {
                Ok(r) => r,
                Err(_) if client.is_closed() => continue 'outer,
                Err(e) => {
                    tracing::warn!(
                        collector = "stats",
                        conn_idx,
                        error = %e,
                        "transient query error, retaining stale data"
                    );
                    continue;
                }
            };

            let now = Instant::now();
            let tps = match prev {
                Some((prev_x, prev_t)) => {
                    let dt = now.duration_since(prev_t).as_secs_f64();
                    if dt > 0.0 {
                        ((raw.xacts - prev_x) as f64 / dt).max(0.0)
                    } else {
                        0.0
                    }
                }
                None => 0.0,
            };
            prev = Some((raw.xacts, now));

            let snapshot = Stats {
                tps,
                active_connections: raw.active_connections,
                cache_hit_pct: raw.cache_hit_pct,
            };

            if try_publish(
                &tx,
                UpdateMessage::Stats { conn_idx, snapshot },
                "stats",
                conn_idx,
            )
            .is_break()
            {
                return;
            }
        }
    }
}
