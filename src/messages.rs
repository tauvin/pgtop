//! Shared `UpdateMessage` channel for multi-connection mode. Every collector
//! and executor publishes through one `mpsc` sender; `conn_idx` addresses
//! the target connection.

use crate::actions::ActionResult;
use crate::app::ConnectionStatus;
use crate::db::{Backend, Lock, Replica, Stats, TopQueriesSnapshot};

/// Message from any collector or executor to the main event loop.
/// `conn_idx` selects the `ConnectionState` inside `App.connections`.
#[derive(Debug)]
pub enum UpdateMessage {
    Activity {
        conn_idx: usize,
        snapshot: Vec<Backend>,
    },
    Locks {
        conn_idx: usize,
        snapshot: Vec<Lock>,
    },
    TopQueries {
        conn_idx: usize,
        snapshot: TopQueriesSnapshot,
    },
    Replication {
        conn_idx: usize,
        snapshot: Vec<Replica>,
    },
    Stats {
        conn_idx: usize,
        snapshot: Stats,
    },
    ActionResult {
        conn_idx: usize,
        result: ActionResult,
    },
    /// Reconnect-state indicator. Published only by the activity collector.
    Status {
        conn_idx: usize,
        status: ConnectionStatus,
    },
}
