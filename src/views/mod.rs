//! Per-tab render functions. Each view receives a `Frame`, target `Rect`,
//! and either `&mut App` or sub-state.

pub mod activity;
pub mod databases;
pub mod locks;
pub mod replication;
pub mod tables;
pub mod top_queries;

pub use activity::render_activity;
pub use databases::render_databases;
pub use locks::render_locks;
pub use replication::render_replication;
pub use tables::render_tables;
pub use top_queries::render_top_queries;
