use crate::config::Config;
use anyhow::Result;
use db::{connect, DB};
use dotenv::dotenv;
use futures::{stream, StreamExt};
use log::{info, warn};
use serde::Serialize;
use skillratings::weng_lin::weng_lin_multi_team;
use skillratings::{
    weng_lin::{WengLinConfig, WengLinRating},
    MultiTeamOutcome,
};
use sqlx;
use std::env;
use std::sync::Arc;
use std::time::Duration;
use task_queue::job::DbQueue;
use task_queue::queue::{Job, Message, MessageScope, Queue};

pub mod config;
pub mod db;

const CONCURRENCY: usize = 1;

pub async fn run() -> Result<()> {
    dotenv().ok();
    env_logger::init();
    let config = Config::new();
    let pool = connect(&env::var("DATABASE_URL").expect("DATABASE_URL must be set"))
        .await
        .expect("Error connecting to database");

    let queue = Arc::new(DbQueue::new(pool.clone()));

    let worker_queue = queue.clone(); // queue is an Arc pointer, so we only copy the reference
    tokio::spawn(async move { run_worker(worker_queue).await });

    if config.event_id.is_some() {
        let queue = Arc::new(DbQueue::new(pool.clone()));
        let job = Message::GenerateRatings {
            event_id: config.event_id.unwrap(),
        };

        let _ = queue.push(job, MessageScope::GenerateRatings, None).await?;
        queue.heartbeat(MessageScope::GenerateRatings).await?;
    } else if config.run {
        queue.heartbeat(MessageScope::GenerateRatings).await?;
    }

    Ok(())
}

async fn run_worker(queue: Arc<dyn Queue>) {
    loop {
        let jobs = match queue
            .pull(MessageScope::GenerateRatings, CONCURRENCY as i32)
            .await
        {
            Ok(jobs) => jobs,
            Err(e) => {
                warn!("Error pulling jobs: {}", e);
                tokio::time::sleep(Duration::from_millis(500)).await;
                Vec::new()
            }
        };

        let number_of_jobs = jobs.len();
        if number_of_jobs > 0 {
            info!("Pulled {} jobs from queue", number_of_jobs);
        }

        stream::iter(jobs)
            .for_each_concurrent(CONCURRENCY, |job| async {
                let job_id = job.id;

                let res = match process_job(job).await {
                    Ok(_) => {
                        info!("Job {} completed", job_id);
                        queue.delete_job(job_id).await
                    }
                    Err(e) => {
                        warn!("Job {} failed: {}", job_id, e);
                        queue.fail_job(job_id).await
                    }
                };

                match res {
                    Ok(_) => {}
                    Err(e) => warn!("deleting / failing job: {}", e),
                }
            })
            .await;
        tokio::time::sleep(Duration::from_millis(125)).await;
    }
}

async fn process_job(job: Job) -> Result<()> {
    let pool = connect(&env::var("DATABASE_URL").expect("DATABASE_URL must be set"))
        .await
        .expect("Error connecting to database");
    let message_clone = job.message.clone();
    match job.message {
        Message::GenerateRatings { event_id } => {
            info!("Processing job: {:?}", message_clone);
            let event_results = get_event(event_id, &pool).await?;
            recalculate_ratings(event_results, &pool).await?;
        }
        _ => {}
    };
    Ok(())
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
struct EventRaceInfoResponse {
    class: String,
    race: String,
}

async fn get_event_races(event: i64, pool: &DB) -> Result<Vec<EventRaceInfoResponse>> {
    let query = "
        SELECT DISTINCT class, race
        FROM event_overall_ranking WHERE event_id = $1
        ";

    let event_results = sqlx::query_as::<_, EventRaceInfoResponse>(query)
        .bind(event)
        .fetch_all(pool)
        .await?;

    Ok(event_results)
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
struct EventResultResponse {
    driver_id: i32,
    event_id: i64,
    class: String,
    race: String,
    rating: Option<i64>,
    position: i32,
}

#[derive(Debug, Clone)]
struct EventResult {
    event_id: i64,
    results: Vec<EventResultResponse>,
}

async fn get_event(event: i64, pool: &DB) -> Result<Vec<EventResult>> {
    let races = get_event_races(event, pool).await?;
    let mut event_results: Vec<EventResult> = Vec::new();
    for race in races {
        let query = "
        SELECT
            driver.id as driver_id,
            event_id,
            eor.class as class,
            race,
            driver.rating,
            position
        FROM event_overall_ranking eor
        LEFT JOIN driver ON
        eor.class = ANY(driver.source_class)
        AND eor.driver_name = ANY(driver.driver_display_name)
        WHERE event_id = $1 AND eor.class = $2 AND race = $3
        ";

        let class = race.class.clone();
        let this_race = race.race.clone();

        let race_result = sqlx::query_as::<_, EventResultResponse>(query)
            .bind(event)
            .bind(class)
            .bind(this_race)
            .fetch_all(pool)
            .await?;

        event_results.push(EventResult {
            event_id: event,
            results: race_result,
        });
    }
    Ok(event_results)
}

async fn recalculate_ratings(event_results: Vec<EventResult>, pool: &DB) -> Result<()> {
    if event_results.len() == 0 {
        return Ok(());
    }
    info!(
        "Recalculating ratings for event {}",
        event_results[0].event_id
    );
    const DEFAULT_UNCERTAINTY: i64 = 25 / 3;
    for event_result in event_results {
        let mut teams_and_ranks: Vec<(Vec<WengLinRating>, MultiTeamOutcome)> = vec![];
        for result in &event_result.results {
            let mut player_rating: Vec<WengLinRating> = vec![];
            player_rating.push(WengLinRating {
                rating: result.rating.unwrap_or(1000) as f64,
                uncertainty: DEFAULT_UNCERTAINTY as f64,
            });
            let outcome = MultiTeamOutcome::new(result.position as usize);
            teams_and_ranks.push((player_rating, outcome));
        }
        let new_ratings = weng_lin_multi_team(
            &teams_and_ranks
                .iter()
                .map(|(rating, outcome)| (rating.as_slice(), *outcome))
                .collect::<Vec<_>>(),
            &WengLinConfig::default(),
        );
        update_ratings(&event_result.clone(), &new_ratings, &pool).await?;
    }
    Ok(())
}

async fn update_ratings(
    event_result: &EventResult,
    new_ratings: &Vec<Vec<WengLinRating>>,
    pool: &DB,
) -> Result<()> {
    for (i, rating) in new_ratings.iter().enumerate() {
        let driver_id = event_result.results[i].driver_id as i32;
        let new_rating = rating[0].rating as i64;
        let query = "
            UPDATE driver
            SET rating = $1
            WHERE id = $2
            ";
        sqlx::query(query)
            .bind(new_rating)
            .bind(driver_id)
            .execute(pool)
            .await?;
    }
    Ok(())
}
