use crate::db::DB;
use anyhow::Result;
use std::sync::Arc;
use task_queue::job::DbQueue;
use task_queue::queue::{Message, MessageScope, Queue};

pub(crate) async fn refresh_ratings(pool: &DB, queue: Arc<DbQueue>) -> Result<()> {
    reset_drivers(pool).await?;
    reset_driver_ratings(pool).await?;
    let events = find_event(pool).await?;
    generate_jobs(events, queue).await?;
    Ok(())
}

async fn reset_drivers(pool: &DB) -> Result<()> {
    let query = "UPDATE driver SET rating = NULL, uncertainty = NULL";
    sqlx::query(query).execute(pool).await?;
    Ok(())
}

async fn reset_driver_ratings(pool: &DB) -> Result<()> {
    let query = "DELETE FROM driver_ratings";
    sqlx::query(query).execute(pool).await?;
    Ok(())
}

#[derive(Debug, sqlx::FromRow)]
struct EventResponse {
    id: i64,
}

async fn find_event(pool: &DB) -> Result<Vec<EventResponse>> {
    let query = "SELECT DISTINCT id FROM event";
    let events = sqlx::query_as(query).fetch_all(pool).await?;
    Ok(events)
}

async fn generate_jobs(events: Vec<EventResponse>, queue: Arc<DbQueue>) -> Result<()> {
    for event in events {
        let job = Message::GenerateRatings { event_id: event.id };
        queue
            .push(job, MessageScope::GenerateRatings, Some(event.id), None)
            .await?
    }
    Ok(())
}
