use async_trait::async_trait;
use std::time::Duration;
use crate::config::AppConfig;
use tokio_rusqlite::Connection;
pub mod process_email;
pub use process_email::ProcessEmail;

#[async_trait]
pub trait PeriodicJob: Send + Sync + 'static {
    /// How often the job should run
    fn interval(&self) -> Duration;

    /// Execute the job
    async fn run_job(&self, config: &AppConfig, db_conn: &Connection);
}

/// Spawns a Tokio task that runs a PeriodicJob on a fixed interval.
pub fn spawn_periodic_job<J>(
    config: AppConfig,
    db_conn: Connection,
    job: J,
) where
    J: PeriodicJob + 'static,
{
    tokio::spawn(async move {
        loop {
            job.run_job(&config, &db_conn).await;
            tokio::time::sleep(job.interval()).await;
        }
    });
}
