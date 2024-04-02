use std::env;
use postgres::db::{connect, DB};
use anyhow::Result;
use sqlx::{query, query_as};
use skillratings::{
    weng_lin::{weng_lin, WengLinConfig, WengLinRating},
    MultiTeamOutcome,
};
use skillratings::weng_lin::weng_lin_multi_team;
use log::info;
use dotenv::dotenv;

pub mod postgres;

pub async fn run() -> Result<()> {
    dotenv().ok();
    env_logger::init();
    let pool = connect(&env::var("DATABASE_URL").expect("DATABASE_URL must be set"))
        .await
        .expect("Error connecting to database");
    let event_id = 430889;
    let event_results = get_event(event_id, &pool).await?;
    recalculate_ratings(event_results)?;
    Ok(())
}

struct EventRaceInfoResponse {
    class: String,
    race: String
}

async fn get_event_races(event: i64, pool: &DB) -> Result<Vec<EventRaceInfoResponse>> {
    let event_results = sqlx::query_as!(
        EventRaceInfoResponse,
        r#"
        SELECT DISTINCT class, race
        FROM event_overall_ranking WHERE event_id = $1
        "#,
        event
        ).fetch_all(pool).await?;
    Ok(event_results)
    }

struct EventResultResponse {
    driver_id: i64,
    event_id: i64,
    class: String,
    race: String,
    rating: Option<i64>,
    position: i64
}

struct EventResult {
    race: EventRaceInfoResponse,
    results: Vec<EventResultResponse>
}

async fn get_event(event: i64, pool: &DB) -> Result<Vec<EventResult>> {
    let races = get_event_races(event, pool).await?;
    let mut event_results: Vec<EventResult> = Vec::new();
    for race in races  {
        let race_result = sqlx::query_as!(
        EventResultResponse,
        r#"
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
        "#,
        event,
        race.class,
        race.race
        ).fetch_all(pool).await?;

        event_results.push(EventResult{race, results: race_result});
    }
    Ok(event_results)
}

fn recalculate_ratings(event_results: Vec<EventResult>) -> Result<()> {
    const DEFAULT_UNCERTAINTY: i64 = 25/3;
    for event_result in event_results {
        let mut teams_and_ranks: Vec<(Vec<WengLinRating>, MultiTeamOutcome)> = vec![];
        for result in event_result.results {
            let mut player_rating: Vec<WengLinRating> = vec![];
            player_rating.push(WengLinRating{rating: result.rating.unwrap_or(1000) as f64, uncertainty: DEFAULT_UNCERTAINTY as f64});
            let outcome = MultiTeamOutcome::new(result.position as usize);
            teams_and_ranks.push((player_rating, outcome));
        }
        let new_ratings = weng_lin_multi_team(&teams_and_ranks.iter().map(|(rating, outcome)| (rating.as_slice(), *outcome)).collect::<Vec<_>>(), &WengLinConfig::default());
        info!("New ratings for {:?} {:?}: {:?}", event_result.race.class, event_result.race.race , new_ratings);
    }
    Ok(())
}