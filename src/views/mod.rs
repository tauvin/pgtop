//! Render-функции для каждого таба. Каждый view получает `Frame`, область
//! `Rect` и `&mut App` (или подсостояние), отрисовывает в эту область.

pub mod activity;
pub mod locks;
pub mod replication;
pub mod top_queries;

pub use activity::render_activity;
pub use locks::render_locks;
pub use replication::render_replication;
pub use top_queries::render_top_queries;
