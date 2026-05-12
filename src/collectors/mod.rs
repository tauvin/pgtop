//! Background tasks that poll Postgres and publish snapshots over a shared
//! `mpsc` channel. Each collector lives in its own submodule with a single
//! `run_*_collector` entry point.

use std::ops::ControlFlow;
use std::time::Duration;

use tokio::{
    sync::mpsc,
    time::{MissedTickBehavior, interval},
};
use tokio_postgres::Client;
use tokio_util::sync::CancellationToken;

use crate::db::{self, DbError};
use crate::messages::UpdateMessage;

/// Publish to the bounded update channel without blocking. On `Closed`
/// the caller should bail out — the UI is gone. On `Full` we drop the
/// message and log it: the UI is slow to drain, but we'd rather skip a
/// snapshot than hold the Postgres connection idle on `send().await`.
pub(crate) fn try_publish(
    tx: &mpsc::Sender<UpdateMessage>,
    msg: UpdateMessage,
    name: &'static str,
    conn_idx: usize,
) -> ControlFlow<()> {
    match tx.try_send(msg) {
        Ok(()) => ControlFlow::Continue(()),
        Err(mpsc::error::TrySendError::Closed(_)) => ControlFlow::Break(()),
        Err(mpsc::error::TrySendError::Full(_)) => {
            tracing::warn!(
                collector = name,
                conn_idx,
                "update channel full, dropping message"
            );
            ControlFlow::Continue(())
        }
    }
}

pub mod activity;
pub mod databases;
pub mod locks;
pub mod replication;
pub mod stats;
pub mod tables;
pub mod top_queries;

pub use activity::run_activity_collector;
pub use databases::run_databases_collector;
pub use locks::run_locks_collector;
pub use replication::run_replication_collector;
pub use stats::run_stats_collector;
pub use tables::run_tables_collector;
pub use top_queries::run_top_queries_collector;

/// Generic connect → tick → fetch → publish loop shared by every
/// stateless collector. Reconnects with backoff on connection loss,
/// logs transient query errors but retains the previous snapshot.
///
/// Stateful collectors (`activity`, `databases`, `stats`) keep their own
/// implementations because they need pre/post-hooks on each tick.
#[allow(clippy::too_many_arguments)]
pub async fn run_simple_collector<F, T>(
    name: &'static str,
    dsn: String,
    tx: mpsc::Sender<UpdateMessage>,
    conn_idx: usize,
    cancel: CancellationToken,
    poll_interval: Duration,
    fetch: F,
    wrap: fn(usize, T) -> UpdateMessage,
) where
    // AsyncFn (stable since 1.85) accepts async fn items directly without
    // the HRTB headaches that Fn + impl Future would bring; the &Client
    // borrow flows cleanly through the inferred lifetime.
    F: AsyncFn(&Client) -> Result<T, DbError>,
{
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
                r = fetch(&client) => r,
            };

            match result {
                Ok(snapshot) => {
                    if try_publish(&tx, wrap(conn_idx, snapshot), name, conn_idx).is_break() {
                        return;
                    }
                }
                Err(_) if client.is_closed() => continue 'outer,
                Err(e) => tracing::warn!(
                    collector = name,
                    conn_idx,
                    error = %e,
                    "transient query error, retaining stale data"
                ),
            }
        }
    }
}
