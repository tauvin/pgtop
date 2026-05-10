//! Phase 8 Block B: shared update-channel для multi-connection mode.
//!
//! Каждый collector / executor publishes свой snapshot через единый
//! `mpsc::UnboundedSender<UpdateMessage>` с `conn_idx`-полем, идентифицирующим
//! целевое соединение. main task держит единственный `Receiver` и в `select!`
//! имеет одну ветку на все updates вместо 5N (5 каналов × N connections).
//!
//! Trade-off vs watch (Phase 5-7): теряем latest-wins coalescing — если UI
//! отстал, snapshot'ы могут накопиться в очереди. Для нашего масштаба
//! (1Hz × 5 collector'ов × ≤5 connections = ~25 msg/sec) нерелевантно.

use crate::actions::ActionResult;
use crate::db::{Backend, Lock, Replica, Stats, TopQueriesSnapshot};

/// Тип сообщения от любого collector'а / executor'а к main event loop'у.
/// `conn_idx` адресует ConnectionState внутри `App.connections`.
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
}
