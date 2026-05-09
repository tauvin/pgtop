//! Фоновые задачи, опрашивающие Postgres и публикующие снапшоты в `watch`-канал.
//!
//! На Phase 3 — единственный сборщик `run_activity_collector`. В Phase 5 здесь
//! появятся ещё `locks`, `top_queries`, `replication`, и каждый со своим каналом
//! и интервалом.

use std::time::Duration;

use tokio::{
    sync::watch,
    time::{MissedTickBehavior, interval},
};
use tokio_postgres::Client;
use tokio_util::sync::CancellationToken;

use crate::db::{self, Backend};

/// Интервал опроса pg_stat_activity. Под Phase 5 интервалы у разных collector'ов
/// будут разные (locks — 1s, top queries — 10s, replication — 5s).
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Запустить сборщик `pg_stat_activity` в текущей tokio-task.
///
/// Контракт:
/// - Опрашивает Postgres каждые `POLL_INTERVAL`.
/// - На каждый успешный fetch — `tx.send(backends)`. `watch` хранит **только
///   последнее** значение (latest-wins): если UI не успевает обработать
///   предыдущий снапшот, он пропадёт без следа — это и нужно для мониторинга.
/// - На fetch-ошибки тихо ignore'им (Phase 4: показывать «fetch failed»
///   в footer'е через Result-обёрнутый snapshot).
///
/// Два сигнала к выходу:
/// 1. `cancel.cancelled()` — внешний сигнал от main: «закрывайся».
///    Преимущество: реакция мгновенная, не ждём следующего тика.
/// 2. `tx.send(...)` вернул `Err` — все `Receiver`'ы дропнуты (UI завершился).
///    Естественный «UI ушёл» signal, на случай если cancel забыли вызвать.
pub async fn run_activity_collector(
    client: Client,
    tx: watch::Sender<Vec<Backend>>,
    cancel: CancellationToken,
) {
    let mut ticker = interval(POLL_INTERVAL);
    // `Delay` — после долгого fetch'а следующий тик ровно через POLL_INTERVAL,
    // не «бэрст» подряд (см. обсуждение MissedTickBehavior в Phase 1).
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        // Ждём следующий тик; cancel может прервать ожидание.
        // `biased;` фиксирует порядок проверки веток на порядок их декларации.
        // Без biased tokio на каждом polling-шаге случайно перемешивает ветки
        // ради fairness; с biased мы гарантируем, что cancel **всегда**
        // проверяется первым — критично для shutdown'а.
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            _ = ticker.tick() => {}
        }

        // Сам fetch тоже cancellable: оборачиваем в select!. Если cancel
        // придёт во время `query`, future fetch_backends дропнется. По доке
        // tokio_postgres это безопасно: серверный запрос продолжит выполняться,
        // но ответ просто будет проигнорирован — соединение в нормальном
        // состоянии для следующих запросов.
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
