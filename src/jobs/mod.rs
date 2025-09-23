use async_trait::async_trait;
use std::time::Duration;
use tokio_rusqlite::Connection;

use crate::config::AppConfig;
pub mod process_email;
pub use process_email::ProcessEmail;
pub mod research_meeting_attendees;
pub use research_meeting_attendees::ResearchMeetingAttendees;

#[async_trait]
pub trait PeriodicJob: Send + Sync + 'static {
    /// How often the job should run
    fn interval(&self) -> Duration;

    /// Execute the job
    async fn run_job(&self, config: &AppConfig, db_conn: &Connection);
}

/// Spawns a Tokio task that runs a PeriodicJob on a fixed interval.
pub fn spawn_periodic_job<J>(config: AppConfig, db_conn: Connection, job: J)
where
    J: PeriodicJob + std::fmt::Debug + 'static,
{
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(job.interval()).await;
            tracing::info!("Starting backgound job: {:?}", job);
            job.run_job(&config, &db_conn).await;
        }
    });
}
