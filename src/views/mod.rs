//! Per-tab render functions. Each view receives a `Frame`, target `Rect`,
//! and either `&mut App` or sub-state.

/// Placeholder rendered for a NULL / unavailable cell. Shared across views
/// so the visual language stays consistent.
pub(super) const EM_DASH: &str = "—";

pub mod activity;
pub mod databases;
pub mod locks;
pub mod replication;
pub mod tables;
pub mod top_queries;
pub mod waits;

pub use activity::render_activity;
pub use databases::render_databases;
pub use locks::render_locks;
pub use replication::render_replication;
pub use tables::render_tables;
pub use top_queries::render_top_queries;
pub use waits::render_waits;
