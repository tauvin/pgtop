//! Фоновые задачи, опрашивающие Postgres и публикующие снапшоты в watch-каналы.
//!
//! Каждый сборщик — отдельный модуль с одной публичной функцией
//! `run_X_collector(client, tx, cancel)`. Phase 5 расширяет набор от единственного
//! `activity` до Activity / Locks / Top Queries / Replication / Stats — каждый
//! со своим интервалом и snapshot-типом.

pub mod activity;
pub mod locks;
pub mod replication;
pub mod stats;
pub mod top_queries;

pub use activity::run_activity_collector;
pub use locks::run_locks_collector;
pub use replication::run_replication_collector;
pub use stats::run_stats_collector;
pub use top_queries::run_top_queries_collector;
