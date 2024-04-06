use std::time::Duration;
use sqlx::{Pool, Postgres};
use sqlx::postgres::PgPoolOptions;

pub type DB = Pool<Postgres>;

pub async fn connect(database_url: &str) -> Result<DB, anyhow::Error> {
    PgPoolOptions::new()
        .max_connections(100)
        .max_lifetime(Duration::from_secs(30 * 60)) // 30 mins
        .connect(database_url)
        .await
        .map_err(|err| anyhow::anyhow!("Error connecting to database: {}", err))
}
