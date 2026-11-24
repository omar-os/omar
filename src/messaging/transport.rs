//! Shared pacing only; provider payloads and delivery semantics stay in adapters.
use anyhow::{ensure, Result};
use std::time::Duration;
use tokio::{
    sync::{Mutex, MutexGuard},
    time::Instant,
};

pub(super) struct RateLimit(Mutex<Instant>);
impl RateLimit {
    pub fn new() -> Self {
        Self(Mutex::new(Instant::now()))
    }
    pub async fn wait(&self) -> MutexGuard<'_, Instant> {
        let next = self.0.lock().await;
        tokio::time::sleep_until(*next).await;
        next
    }
    pub fn defer(next: &mut Instant, seconds: f64) -> Result<()> {
        ensure!(
            seconds.is_finite() && (0.0..=86400.0).contains(&seconds),
            "Invalid API rate limit"
        );
        *next = Instant::now() + Duration::from_secs_f64(seconds + 0.1);
        Ok(())
    }
}
