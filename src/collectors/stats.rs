//! Сборщик сводных метрик для шапки: TPS, active connections, cache hit %.
//! Stateful — держит prev-snapshot для diff'а TPS.
//!
//! Phase 8 Block C: silent reconnect. На реконнекте сбрасываем prev-state —
//! diff между до-disconnect и после был бы бессмысленным (counter Postgres
//! не сбрасывается, но временной разрыв >> poll_interval даст ложно-низкий
//! TPS на первом тике после reconnect'а).

use std::time::{Duration, Instant};

use tokio::{
    sync::mpsc,
    time::{MissedTickBehavior, interval},
};
use tokio_util::sync::CancellationToken;

use crate::db::{self, Stats};
use crate::messages::UpdateMessage;

pub async fn run_stats_collector(
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

        // Per-connection-instance state: на reconnect'е стартуем с None.
        let mut prev_xacts: Option<i64> = None;
        let mut prev_time: Option<Instant> = None;

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
                Err(_) => continue,
            };

            let now = Instant::now();
            let tps = match (prev_xacts, prev_time) {
                (Some(prev_x), Some(prev_t)) => {
                    let dt = now.duration_since(prev_t).as_secs_f64();
                    if dt > 0.0 {
                        ((raw.xacts - prev_x) as f64 / dt).max(0.0)
                    } else {
                        0.0
                    }
                }
                _ => 0.0,
            };

            prev_xacts = Some(raw.xacts);
            prev_time = Some(now);

            let snapshot = Stats {
                tps,
                active_connections: raw.active_connections,
                cache_hit_pct: raw.cache_hit_pct,
            };

            if tx
                .send(UpdateMessage::Stats { conn_idx, snapshot })
                .is_err()
            {
                return;
            }
        }
    }
}
