//! Action executor: a separate tokio task that runs cancel/terminate
//! commands on a dedicated Postgres connection. All events are logged via
//! `tracing` with `target: "audit"`.

use chrono::{DateTime, Utc};
use tokio::sync::mpsc;
use tokio_postgres::Client;
use tokio_util::sync::CancellationToken;

use crate::db;
use crate::messages::UpdateMessage;

/// A command for the executor.
#[derive(Debug, Clone)]
pub enum ActionCommand {
    /// `pg_cancel_backend(pid)` — cancels the current query, keeps the session.
    Cancel { pid: i32 },
    /// `pg_terminate_backend(pid)` — kills the entire session.
    Terminate { pid: i32 },
}

impl ActionCommand {
    pub fn pid(&self) -> i32 {
        match self {
            Self::Cancel { pid } | Self::Terminate { pid } => *pid,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Cancel { .. } => "cancel",
            Self::Terminate { .. } => "terminate",
        }
    }
}

/// Result of executing an action, published to the UI.
///
/// `outcome`:
/// - `Ok(true)` — the function returned `true`: the signal was sent.
/// - `Ok(false)` — the function returned `false`: pid does not exist or the
///   caller lacks privilege. Postgres does not distinguish these two cases.
/// - `Err(String)` — a SQL error (connection, syntax, etc.).
#[derive(Debug, Clone)]
pub struct ActionResult {
    pub command: ActionCommand,
    pub outcome: Result<bool, String>,
    #[allow(dead_code)]
    pub at: DateTime<Utc>,
}

/// Run the action executor on a spawned task. Owns its connection: connects
/// with backoff on startup and reconnects on demand if the client closed.
pub async fn run_action_executor(
    dsn: String,
    mut commands_rx: mpsc::UnboundedReceiver<ActionCommand>,
    update_tx: mpsc::UnboundedSender<UpdateMessage>,
    conn_idx: usize,
    cancel: CancellationToken,
) {
    let mut client = match db::connect_with_backoff(&dsn, &cancel, |_| {}).await {
        Some(c) => c,
        None => return,
    };

    loop {
        let cmd = tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            c = commands_rx.recv() => match c {
                Some(c) => c,
                None => break,
            },
        };

        if client.is_closed() {
            client = match db::connect_with_backoff(&dsn, &cancel, |_| {}).await {
                Some(c) => c,
                None => return,
            };
        }

        let outcome = execute(&client, &cmd).await;
        log_audit(&cmd, &outcome);

        let result = ActionResult {
            command: cmd,
            outcome,
            at: Utc::now(),
        };

        if update_tx
            .send(UpdateMessage::ActionResult { conn_idx, result })
            .is_err()
        {
            break;
        }
    }
}

async fn execute(client: &Client, cmd: &ActionCommand) -> Result<bool, String> {
    let (sql, pid) = match cmd {
        ActionCommand::Cancel { pid } => ("SELECT pg_cancel_backend($1)", *pid),
        ActionCommand::Terminate { pid } => ("SELECT pg_terminate_backend($1)", *pid),
    };

    let row = client
        .query_one(sql, &[&pid])
        .await
        .map_err(|e| e.to_string())?;
    Ok(row.get(0))
}

fn log_audit(cmd: &ActionCommand, outcome: &Result<bool, String>) {
    match outcome {
        Ok(true) => tracing::info!(
            target: "audit",
            action = cmd.label(),
            pid = cmd.pid(),
            "action executed successfully"
        ),
        Ok(false) => tracing::warn!(
            target: "audit",
            action = cmd.label(),
            pid = cmd.pid(),
            "action returned false (no such backend or insufficient permission)"
        ),
        Err(e) => tracing::error!(
            target: "audit",
            action = cmd.label(),
            pid = cmd.pid(),
            error = %e,
            "action failed with SQL error"
        ),
    }
}
