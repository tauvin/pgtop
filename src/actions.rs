//! Action executor — отдельная tokio-task, исполняющая cancel/terminate-команды
//! на дедикейтед-соединении.
//!
//! Архитектура:
//! - main task шлёт `ActionCommand` через mpsc::UnboundedSender (try_send,
//!   не-блокирующий — event loop не блокируется на ожидании).
//! - executor читает команды из mpsc::UnboundedReceiver.
//! - SQL выполняется на собственном `Client` (отдельное соединение) —
//!   не сериализуется через collector-driver'ы.
//! - результат публикуется в watch::channel<Option<ActionResult>>.
//! - main task ловит `result_rx.changed()` в select! и обновляет UI.
//!
//! Все события логируются через `tracing` с `target: "audit"` — в audit-log
//! файл попадают timestamp + команда + результат.

use chrono::{DateTime, Utc};
use tokio::sync::mpsc;
use tokio_postgres::Client;
use tokio_util::sync::CancellationToken;

use crate::messages::UpdateMessage;

/// Команда для executor'а.
#[derive(Debug, Clone)]
pub enum ActionCommand {
    /// `pg_cancel_backend(pid)` — отменяет текущий запрос; сессия остаётся.
    Cancel { pid: i32 },
    /// `pg_terminate_backend(pid)` — обрывает всю сессию.
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

/// Результат исполнения action'а — то, что executor публикует в UI.
///
/// `outcome`:
/// - `Ok(true)` — `pg_cancel_backend` вернул `true`: сигнал успешно послан.
/// - `Ok(false)` — `pg_cancel_backend` вернул `false`: pid не существует
///   ИЛИ у нас нет привилегии. Postgres не различает эти два случая
///   в возвращаемом значении.
/// - `Err(String)` — собственно SQL-ошибка (соединение, синтаксис, и т.п.).
#[derive(Debug, Clone)]
pub struct ActionResult {
    pub command: ActionCommand,
    pub outcome: Result<bool, String>,
    /// Timestamp выполнения. UI пока не отображает (Phase 7 — relative
    /// «N ago»), но в логах audit-target видно через Debug-форматирование.
    #[allow(dead_code)]
    pub at: DateTime<Utc>,
}

/// Запустить executor в spawned-таске. Phase 8 Block B: результат публикуется
/// через shared `mpsc::UnboundedSender<UpdateMessage>` с `conn_idx` —
/// единая mpsc fan-in архитектура для всех collector'ов и executor'ов.
pub async fn run_action_executor(
    client: Client,
    mut commands_rx: mpsc::UnboundedReceiver<ActionCommand>,
    update_tx: mpsc::UnboundedSender<UpdateMessage>,
    conn_idx: usize,
    cancel: CancellationToken,
) {
    loop {
        let cmd = tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            c = commands_rx.recv() => match c {
                Some(c) => c,
                None => break,
            },
        };

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

/// Audit-event в файл: уровень info при success, warn при выполненной но
/// безрезультатной команде (false), error при SQL-ошибке.
/// `target: "audit"` позволяет фильтровать через RUST_LOG=audit=info отдельно.
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
