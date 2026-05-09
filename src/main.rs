use std::env;

use color_eyre::eyre::{Context, Result};
use tokio_postgres::NoTls;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

const DEFAULT_DSN: &str = "postgres://pgtop:pgtop@localhost:5433/pgtop";

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let dsn = env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DSN.to_string());
    info!("connecting to postgres");

    // Rust-специфика: tokio_postgres::connect возвращает пару (Client, Connection).
    // Client — это handle для запросов; Connection — самостоятельный Future, который
    // фактически читает и пишет в TCP-сокет. Если её не запустить параллельно (через
    // tokio::spawn), client просто зависнет, потому что некому читать ответы из сокета.
    let (client, connection) = tokio_postgres::connect(&dsn, NoTls)
        .await
        .wrap_err("failed to connect to postgres")?;

    // `move` забирает `connection` в замыкание — это владеющая Future, у неё нет лайфтайма,
    // привязанного к main, поэтому её можно безопасно гонять в отдельной таске.
    // Дополнительно tokio::spawn требует Future: Send + 'static — и connection это удовлетворяет.
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            error!("postgres connection error: {e}");
        }
    });

    // tokio_postgres::Row не выводит тип из контекста сам — нужно явно указать,
    // во что распаковывать колонку. Делаем через аннотацию `let v: T`,
    // эквивалент row.get::<_, String>(0) с turbofish.
    let row = client
        .query_one("SELECT version()", &[])
        .await
        .wrap_err("SELECT version() failed")?;
    let version: String = row.get(0);
    info!("postgres version: {version}");

    let row = client.query_one("SELECT current_database()", &[]).await?;
    let database: String = row.get(0);
    info!("connected to database: {database}");

    Ok(())
}
