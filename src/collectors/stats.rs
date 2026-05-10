//! Сборщик сводных метрик для шапки: TPS, active connections, cache hit %.
//!
//! TPS вычисляется как дельта `xact_commit + xact_rollback` между двумя
//! snapshot'ами, поделённая на прошедшее время. Collector держит state
//! локально (predыдущий snapshot + момент времени) — это отличает его
//! от других collector'ов проекта, которые stateless.

use std::time::{Duration, Instant};

use tokio::{
    sync::watch,
    time::{MissedTickBehavior, interval},
};
use tokio_postgres::Client;
use tokio_util::sync::CancellationToken;

use crate::db::{self, Stats};

const POLL_INTERVAL: Duration = Duration::from_secs(1);

pub async fn run_stats_collector(
    client: Client,
    tx: watch::Sender<Stats>,
    cancel: CancellationToken,
) {
    let mut ticker = interval(POLL_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    // Local state — сохраняется между итерациями цикла.
    // На первом tick'е prev = None → TPS = 0.0 (нет diff'а), но snapshot
    // запишется в prev для следующего вычисления.
    let mut prev_xacts: Option<i64> = None;
    let mut prev_time: Option<Instant> = None;

    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            _ = ticker.tick() => {}
        }

        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            r = db::fetch_raw_stats(&client) => r,
        };

        let raw = match result {
            Ok(r) => r,
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
            // Первый tick — ещё нет prev для diff'а.
            _ => 0.0,
        };

        prev_xacts = Some(raw.xacts);
        prev_time = Some(now);

        let stats = Stats {
            tps,
            active_connections: raw.active_connections,
            cache_hit_pct: raw.cache_hit_pct,
        };

        if tx.send(stats).is_err() {
            break;
        }
    }
}
