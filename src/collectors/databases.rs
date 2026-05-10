//! `pg_stat_database` collector. Silent reconnect, default 5s interval.
//! Computes per-database TPS from the delta between consecutive snapshots
//! so the first sample shows `—` and every subsequent one shows a rate.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use tokio::{
    sync::mpsc,
    time::{MissedTickBehavior, interval},
};
use tokio_util::sync::CancellationToken;

use crate::db::{self, DatabaseStat};
use crate::messages::UpdateMessage;

/// Prior commit+rollback total and the wall-clock instant at which it
/// was observed, keyed by database name. Used to derive TPS as
/// `Δ(commits+rollbacks) / Δt`.
type PrevTotals = HashMap<String, (i64, Instant)>;

pub async fn run_databases_collector(
    dsn: String,
    tx: mpsc::UnboundedSender<UpdateMessage>,
    conn_idx: usize,
    cancel: CancellationToken,
    poll_interval: Duration,
) {
    let mut prev: PrevTotals = HashMap::new();

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
                r = db::fetch_database_stats(&client) => r,
            };

            match result {
                Ok(mut snapshot) => {
                    fill_tps(&mut snapshot, &mut prev);
                    if tx
                        .send(UpdateMessage::Databases { conn_idx, snapshot })
                        .is_err()
                    {
                        return;
                    }
                }
                Err(_) if client.is_closed() => continue 'outer,
                Err(e) => tracing::warn!(
                    collector = "databases",
                    conn_idx,
                    error = %e,
                    "transient query error, retaining stale data"
                ),
            }
        }
    }
}

fn fill_tps(snapshot: &mut [DatabaseStat], prev: &mut PrevTotals) {
    let now = Instant::now();
    for db in snapshot.iter_mut() {
        let total = db.xact_commit.saturating_add(db.xact_rollback);
        if let Some((prev_total, prev_at)) = prev.get(&db.datname) {
            let elapsed = now.duration_since(*prev_at).as_secs_f64();
            // pg_stat_reset() or DB drop+recreate can step the counter
            // backwards — surface the next valid sample as the new
            // baseline instead of reporting a negative rate.
            if elapsed > 0.0 && total >= *prev_total {
                db.tps = Some((total - prev_total) as f64 / elapsed);
            }
        }
        prev.insert(db.datname.clone(), (total, now));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat(datname: &str, commit: i64, rollback: i64) -> DatabaseStat {
        DatabaseStat {
            datname: datname.into(),
            numbackends: 0,
            xact_commit: commit,
            xact_rollback: rollback,
            blks_hit: 0,
            blks_read: 0,
            temp_bytes: 0,
            deadlocks: 0,
            tps: None,
        }
    }

    #[test]
    fn first_sample_has_no_tps() {
        let mut prev = PrevTotals::new();
        let mut snap = vec![stat("app", 100, 0)];
        fill_tps(&mut snap, &mut prev);
        assert!(snap[0].tps.is_none());
        assert!(prev.contains_key("app"));
    }

    #[test]
    fn second_sample_yields_positive_rate() {
        let mut prev = PrevTotals::new();
        prev.insert(
            "app".to_string(),
            (100, Instant::now() - Duration::from_secs(10)),
        );
        let mut snap = vec![stat("app", 200, 0)];
        fill_tps(&mut snap, &mut prev);
        let tps = snap[0].tps.expect("tps should be populated");
        // 100 txns over ~10s ≈ 10 TPS; allow slack for elapsed jitter.
        assert!(
            (8.0..12.0).contains(&tps),
            "tps out of expected range: {tps}"
        );
    }

    #[test]
    fn counter_reset_drops_to_none_for_one_sample() {
        let mut prev = PrevTotals::new();
        prev.insert(
            "app".to_string(),
            (500, Instant::now() - Duration::from_secs(5)),
        );
        let mut snap = vec![stat("app", 10, 0)];
        fill_tps(&mut snap, &mut prev);
        assert!(snap[0].tps.is_none());
        // Baseline rebuilt; next sample will produce a rate.
        assert_eq!(prev["app"].0, 10);
    }
}
