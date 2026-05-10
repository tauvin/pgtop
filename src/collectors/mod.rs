//! Background tasks that poll Postgres and publish snapshots over a shared
//! `mpsc` channel. Each collector lives in its own submodule with a single
//! `run_*_collector` entry point.

pub mod activity;
pub mod databases;
pub mod locks;
pub mod replication;
pub mod stats;
pub mod top_queries;

pub use activity::run_activity_collector;
pub use databases::run_databases_collector;
pub use locks::run_locks_collector;
pub use replication::run_replication_collector;
pub use stats::run_stats_collector;
pub use top_queries::run_top_queries_collector;
