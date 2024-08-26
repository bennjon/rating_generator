use crate::config::Config;
use anyhow::Result;
use db::{connect, DB};
use dotenv::dotenv;
use futures::{stream, StreamExt};
use log::{error, info, warn};
use serde::Serialize;
use skillratings::{
    weng_lin::{weng_lin_multi_team, WengLinConfig, WengLinRating},
    MultiTeamOutcome,
};
use std::env;
use std::sync::Arc;
use std::time::Duration;
use task_queue::job::DbQueue;
use task_queue::queue::{Job, Message, MessageScope, Queue};

pub mod config;
pub mod db;
mod refresh;

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

    let queue = Arc::new(DbQueue::new(pool.clone()));

    if config.event_id.is_some() {
        let event_id = config.event_id.unwrap();
        let job = Message::GenerateRatings { event_id };

        queue
            .clone()
            .push(job, MessageScope::GenerateRatings, Some(event_id), None)
            .await?
    } else if config.refresh {
        refresh::refresh_ratings(&pool, queue.clone()).await?;
    }
    queue.heartbeat(MessageScope::GenerateRatings).await?;
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

                let _res = match process_job(job).await {
                    Ok(_) => {
                        info!("Job {} completed", job_id);
                        queue.delete_job(job_id).await
                    }
                    Err(e) => {
                        error!("Job {} failed: {}", job_id, e);
                        queue.fail_job(job_id).await
                    }
                };
            })
            .await;
        tokio::time::sleep(Duration::from_millis(125)).await;
    }
}

async fn process_job(job: Job) -> Result<()> {
    let pool = connect(&env::var("DATABASE_URL")?).await?;
    let message_clone = job.message.clone();
    if let Message::GenerateRatings { event_id } = job.message {
        info!("Processing job: {:?}", message_clone);
        let event_results = get_event(event_id, &pool).await?;
        recalculate_ratings(event_results, pool).await?;
    };
    Ok(())
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
struct EventRaceInfoResponse {
    class: String,
}

async fn get_event_classes(event: i64, pool: &DB) -> Result<Vec<EventRaceInfoResponse>> {
    let query = "
        SELECT DISTINCT class
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
    rating: Option<f64>,
    uncertainty: Option<f64>,
    position: i32,
    start_date: String,
}

#[derive(Debug, Clone)]
struct ClassResult {
    name: String,
    results: Vec<EventResultResponse>,
}

#[derive(Debug, Clone)]
struct EventResult {
    event_id: i64,
    results: Vec<ClassResult>,
}

async fn get_event(event: i64, pool: &DB) -> Result<EventResult> {
    let classes = get_event_classes(event, pool).await?;
    let mut race_results: Vec<ClassResult> = Vec::new();
    for class in classes {
        let query = "
        SELECT
            driver.id as driver_id,
            event_id,
            eor.class as class,
            driver.rating,
            driver.uncertainty,
            position,
            event.start_date
        FROM event_overall_ranking eor
        LEFT JOIN driver ON
        eor.class = ANY(driver.source_class)
        AND eor.driver_name = ANY(driver.driver_display_name)
        LEFT JOIN event ON
        eor.event_id = event.id
        WHERE event_id = $1 AND eor.class = $2
        ";

        let class = class.class.clone();

        let race_result = sqlx::query_as::<_, EventResultResponse>(query)
            .bind(event)
            .bind(class.clone())
            .fetch_all(pool)
            .await?;

        race_results.push(ClassResult {
            name: class,
            results: race_result,
        });
    }
    let event_result = EventResult {
        event_id: event,
        results: race_results
    };
    Ok(event_result)
}

async fn recalculate_ratings(event_results: EventResult, pool: DB) -> Result<()> {
    if event_results.results.is_empty() {
        return Ok(());
    }
    info!(
        "Recalculating ratings for event {} for {} classes",
        event_results.event_id,
        event_results.results.len()
    );

    for class_result in event_results.results {
        let mut teams_and_ranks: Vec<(Vec<WengLinRating>, MultiTeamOutcome)> = vec![];
        let config = WengLinConfig{
            beta: (class_result.results.len() * 2) as f64,
            uncertainty_tolerance: 0.001,
        };
        let default_rating = WengLinRating{
            rating: 25.0,
            uncertainty: 25.0 / 3.0,
        };
        for result in &class_result.results {
            let mut player_rating: Vec<WengLinRating> = vec![];
            match result.rating {
                Some(_) => {
                    player_rating.push(WengLinRating {
                        rating: result.rating.unwrap() as f64,
                        uncertainty: result.uncertainty.unwrap(),
                    });
                }
                None => {
                    player_rating.push(default_rating.clone());
                }
            }
            let outcome = MultiTeamOutcome::new(result.position as usize);
            teams_and_ranks.push((player_rating, outcome));
        }
        let new_ratings = weng_lin_multi_team(
            &teams_and_ranks
                .iter()
                .map(|(rating, outcome)| (rating.as_slice(), *outcome))
                .collect::<Vec<_>>(),
            &config,
        );
        update_ratings(&class_result.clone(),event_results.event_id.clone(), &new_ratings, pool.clone()).await?;
    }
    Ok(())
}

async fn update_ratings(
    class_result: &ClassResult,
    event_id: i64,
    new_ratings: &[Vec<WengLinRating>],
    pool: DB,
) -> Result<()> {
    for (i, rating) in new_ratings.iter().enumerate() {
        let driver_id = class_result.results[i].driver_id;
        let new_driver_rating = calculate_driver_rating(
            driver_id,
            class_result.name.clone(),
            rating[0].rating.clone(),
            class_result.results[i].start_date.clone(),
            &pool,
        ).await?;
        let player_uncertainty = rating[0].uncertainty;
        let query = "
            UPDATE driver
            SET rating = $1,
            uncertainty = $2
            WHERE id = $3
            ";
        sqlx::query(query)
            .bind(new_driver_rating)
            .bind(player_uncertainty)
            .bind(driver_id)
            .execute(&pool)
            .await?;
        log_rating(
            RatingLogRequest {
                driver_id,
                event_id,
                event_start_date: class_result.results[i].start_date.clone(),
                event_rating: rating[0].rating.clone(),
                driver_rating: new_driver_rating,
                uncertainty: player_uncertainty,
                class: class_result.name.clone(),
                rating_ts: chrono::Utc::now(),
            },
            &pool,
        )
        .await?;
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
struct RollingDriverRatingResponse {
    event_rating: f64
}

async fn calculate_driver_rating(
    driver_id: i32,
    class: String,
    latest_event_rating: f64,
    latest_event_date: String,
    pool: &DB,
) -> Result<f64> {
    let query = "
        SELECT
            dr.event_rating
        FROM driver_ratings dr
        WHERE dr.driver_id = $2
        AND TO_DATE(dr.event_start_date, 'YYYY-MM-DD') >= TO_DATE($3, 'YYYY-MM-DD') - INTERVAL '1 year'
        AND TO_DATE(dr.event_start_date, 'YYYY-MM-DD') < TO_DATE($3, 'YYYY-MM-DD')
        ";
    let rolling_year_average = sqlx::query_as::<_, RollingDriverRatingResponse>(query)
        .bind(class)
        .bind(driver_id)
        .bind(latest_event_date.clone())
        .fetch_all(pool)
        .await?;
    let mut ratings: Vec<f64> = rolling_year_average.into_iter().map(|r| r.event_rating).collect();

    ratings.push(latest_event_rating);

    let sum: f64 = ratings.iter().sum();
    let count = ratings.len() as f64;
    let average = sum / count;

    Ok(average)
}

struct RatingLogRequest {
    driver_id: i32,
    event_id: i64,
    event_start_date: String,
    event_rating: f64,
    driver_rating: f64,
    uncertainty: f64,
    class: String,
    rating_ts: chrono::DateTime<chrono::Utc>,
}

async fn log_rating(request: RatingLogRequest, pool: &DB) -> Result<()> {
    let query = "
        INSERT INTO driver_ratings (driver_id, event_id, event_start_date, event_rating, driver_rating, uncertainty, class, rating_ts)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        ";
    sqlx::query(query)
        .bind(request.driver_id)
        .bind(request.event_id)
        .bind(request.event_start_date)
        .bind(request.event_rating)
        .bind(request.driver_rating)
        .bind(request.uncertainty)
        .bind(request.class)
        .bind(request.rating_ts)
        .execute(pool)
        .await?;
    Ok(())
}
