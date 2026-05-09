use std::env;

use color_eyre::eyre::Result;
use tabled::{Table, settings::Style};
use tokio::time::{Duration, MissedTickBehavior};
use tracing_subscriber::EnvFilter;

mod db;

const DEFAULT_DSN: &str = "postgres://pgtop:pgtop@localhost:5433/pgtop";

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let dsn = env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    let client = db::connect(&dsn).await?;

    // tokio::time::interval vs sleep — почему здесь interval:
    //
    // - `sleep(1s)` внутри loop даёт цикл длиной (работа + 1s), без выравнивания.
    //   Если fetch занимает 200ms — реальный период 1.2s; если БД лагает на 800ms —
    //   1.8s. Дрейф накапливается линейно: за час «секундный» цикл может проскочить
    //   на десятки тиков.
    //
    // - `interval` хранит внутри расписание тиков и сама ждёт ровно столько,
    //   чтобы `tick().await` проснулся в назначенный момент. Если работа уложилась
    //   в период — следующий тик ровно через секунду от старта предыдущего.
    //
    // Что делать, когда работа дольше периода — настраивается MissedTickBehavior:
    //
    // - Burst (default): по возвращении выстреливает все пропущенные тики подряд
    //   без пауз. Для мониторинга катастрофично — после лагающего fetch'а получим
    //   бэрст fetch'ей подряд и сами же добавим нагрузки.
    // - Skip: пропущенные тики выкидываются, следующий — на ближайшем «слоте»
    //   расписания от старта. Расписание сохраняет абсолютную фазу.
    // - Delay: расписание сдвигается — следующий тик ровно через period после того,
    //   как `tick().await` вернётся. Фаза «плывёт», но расстояние между фактическими
    //   тиками стабильно ≥ period.
    //
    // Для опроса БД важно ≥1с между запросами; абсолютная фаза не нужна → Delay.
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    // tokio::select! гоняет несколько Future параллельно и заходит в ветку первой
    // готовой. Rust-специфика: когда одна ветка выигрывает, **остальные Future
    // дропаются** прямо в середине своего `.await` — их state-machine уничтожается,
    // async-код в них больше не запустится. Это **не** `Promise.race` из JS, где
    // проигравшие промисы продолжают крутиться и могут оставить «хвостовые»
    // эффекты. Отсюда — понятие cancellation safety: безопасно ли дропнуть
    // данный future в произвольной await-точке без порчи общего состояния.
    // Все ветки ниже cancel-safe; пояснения — рядом с каждой.
    loop {
        tokio::select! {
            // `ticker.tick()` cancel-safe: при дропе расписание Interval остаётся
            // на месте, следующий вызов в новой итерации loop подхватит точно
            // в нужный момент. Первый `tick()` резолвится сразу, без ожидания
            // в один период; для стартовой задержки есть `interval_at(start, period)`.
            _ = ticker.tick() => {
                // ВАЖНО: fetch_backends — в **теле** ветки, а не внутри select!.
                // Как только тик выиграл, тело уже ни с кем не гоняется; `.await`
                // на fetch_backends прервать Ctrl+C нельзя — мы вернёмся
                // в select! только после возврата из fetch_backends. Для Phase 1
                // это нормально (запрос быстрый). Phase 3 — обернём долгие
                // операции в отдельный select! / CancellationToken.
                let backends = db::fetch_backends(&client).await?;
                let mut table = Table::new(&backends);
                table.with(Style::psql());
                println!("{table}");
            }
            // `tokio::signal::ctrl_c()` ставит OS-handler на SIGINT (UNIX) /
            // CTRL_C_EVENT (Windows) и резолвится при сигнале. Cancel-safe:
            // дроп снимает handler, повторный вызов ставит его снова.
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Ctrl+C received, shutting down");
                break;
            }
        }
    }

    Ok(())
}
