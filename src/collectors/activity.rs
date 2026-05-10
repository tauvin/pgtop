//! Сборщик `pg_stat_activity`: опрашивает раз в секунду, публикует
//! `Vec<Backend>` через `watch::channel`.

use std::time::Duration;

use tokio::{
    sync::watch,
    time::{MissedTickBehavior, interval},
};
use tokio_postgres::Client;
use tokio_util::sync::CancellationToken;

use crate::db::{self, Backend};

const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Запустить сборщик `pg_stat_activity` в текущей tokio-task.
///
/// Контракт:
/// - Опрашивает Postgres каждые `POLL_INTERVAL`.
/// - На каждый успешный fetch — `tx.send(backends)`. `watch` хранит **только
///   последнее** значение (latest-wins): если UI не успевает обработать
///   предыдущий снапшот, он пропадёт без следа — это и нужно для мониторинга.
/// - На fetch-ошибки тихо ignore'им. Будущая итерация — публиковать `Result`
///   через канал, чтобы UI отрисовал banner.
///
/// Два сигнала к выходу:
/// 1. `cancel.cancelled()` — внешний сигнал от main: «закрывайся».
/// 2. `tx.send(...)` вернул `Err` — все Receiver'ы дропнуты (UI завершился).
pub async fn run_activity_collector(
    client: Client,
    tx: watch::Sender<Vec<Backend>>,
    cancel: CancellationToken,
) {
    let mut ticker = interval(POLL_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        // `biased;` — cancel ВСЕГДА проверяется первым (без него tokio
        // случайно перемешивает порядок ради fairness, и cancel может
        // пропустить ход). Критично для shutdown.
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            _ = ticker.tick() => {}
        }

        // Сам fetch тоже cancellable. tokio_postgres::Client::query cancel-safe:
        // drop future оставляет соединение в нормальном состоянии (серверный
        // запрос продолжит выполняться, ответ просто будет проигнорирован).
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            r = db::fetch_backends(&client) => r,
        };

        match result {
            Ok(backends) => {
                if tx.send(backends).is_err() {
                    break; // UI ушёл — нам тоже пора
                }
            }
            Err(_) => continue, // следующий тик попробует снова
        }
    }
}
