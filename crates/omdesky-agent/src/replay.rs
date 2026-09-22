use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use time::OffsetDateTime;
use tokio::sync::Mutex;
use uuid::Uuid;

pub const REQUEST_WINDOW: Duration = Duration::from_secs(120);
pub const MAX_CLOCK_SKEW: Duration = Duration::from_secs(120);
pub const MAX_TRACKED_REQUESTS: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FreshnessError {
    Missing,
    Stale,
    Replayed,
    Saturated,
}

impl FreshnessError {
    pub fn code(self) -> &'static str {
        match self {
            FreshnessError::Missing => "REQUEST_ID_REQUIRED",
            FreshnessError::Stale => "REQUEST_STALE",
            FreshnessError::Replayed => "REQUEST_REPLAYED",
            FreshnessError::Saturated => "REQUEST_TRACKING_SATURATED",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            FreshnessError::Missing => "This request is missing its identifier.",
            FreshnessError::Stale => "This request is too old to apply.",
            FreshnessError::Replayed => "This request was already applied.",
            FreshnessError::Saturated => "Too many requests are in flight. Please try again.",
        }
    }
}

pub struct ReplayGuard {
    seen: Mutex<HashMap<Uuid, Instant>>,
    window: Duration,
    skew: Duration,
    capacity: usize,
}

impl Default for ReplayGuard {
    fn default() -> Self {
        Self::new(REQUEST_WINDOW, MAX_CLOCK_SKEW, MAX_TRACKED_REQUESTS)
    }
}

impl ReplayGuard {
    pub fn new(window: Duration, skew: Duration, capacity: usize) -> Self {
        Self {
            seen: Mutex::new(HashMap::new()),
            window,
            skew,
            capacity,
        }
    }

    pub async fn admit(
        &self,
        request_id: Option<Uuid>,
        issued_at: Option<OffsetDateTime>,
    ) -> Result<(), FreshnessError> {
        let (Some(request_id), Some(issued_at)) = (request_id, issued_at) else {
            return Err(FreshnessError::Missing);
        };

        let age = OffsetDateTime::now_utc() - issued_at;

        if age.abs().unsigned_abs() > self.skew {
            return Err(FreshnessError::Stale);
        }

        let now = Instant::now();
        let mut seen = self.seen.lock().await;
        seen.retain(|_, recorded| now.duration_since(*recorded) < self.window);

        if seen.len() >= self.capacity {
            return Err(FreshnessError::Saturated);
        }

        if seen.insert(request_id, now).is_some() {
            return Err(FreshnessError::Replayed);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_a_fresh_request_is_admitted_once() {
        let guard = ReplayGuard::default();
        let request_id = Uuid::new_v4();
        let issued_at = OffsetDateTime::now_utc();

        assert_eq!(guard.admit(Some(request_id), Some(issued_at)).await, Ok(()));
        assert_eq!(
            guard.admit(Some(request_id), Some(issued_at)).await,
            Err(FreshnessError::Replayed)
        );
    }

    #[tokio::test]
    async fn test_a_request_without_an_identifier_is_rejected() {
        let guard = ReplayGuard::default();

        assert_eq!(
            guard.admit(None, Some(OffsetDateTime::now_utc())).await,
            Err(FreshnessError::Missing)
        );
        assert_eq!(
            guard.admit(Some(Uuid::new_v4()), None).await,
            Err(FreshnessError::Missing)
        );
    }

    #[tokio::test]
    async fn test_a_stale_or_future_request_is_rejected() {
        let guard = ReplayGuard::default();
        let old = OffsetDateTime::now_utc() - time::Duration::minutes(10);
        let ahead = OffsetDateTime::now_utc() + time::Duration::minutes(10);

        assert_eq!(
            guard.admit(Some(Uuid::new_v4()), Some(old)).await,
            Err(FreshnessError::Stale)
        );
        assert_eq!(
            guard.admit(Some(Uuid::new_v4()), Some(ahead)).await,
            Err(FreshnessError::Stale)
        );
    }

    #[tokio::test]
    async fn test_tracking_is_bounded() {
        let guard = ReplayGuard::new(REQUEST_WINDOW, MAX_CLOCK_SKEW, 2);
        let issued_at = OffsetDateTime::now_utc();

        assert_eq!(
            guard.admit(Some(Uuid::new_v4()), Some(issued_at)).await,
            Ok(())
        );
        assert_eq!(
            guard.admit(Some(Uuid::new_v4()), Some(issued_at)).await,
            Ok(())
        );
        assert_eq!(
            guard.admit(Some(Uuid::new_v4()), Some(issued_at)).await,
            Err(FreshnessError::Saturated)
        );
    }

    #[tokio::test]
    async fn test_identifiers_are_forgotten_after_the_window() {
        let guard = ReplayGuard::new(Duration::from_millis(1), MAX_CLOCK_SKEW, 8);
        let request_id = Uuid::new_v4();
        let issued_at = OffsetDateTime::now_utc();

        assert_eq!(guard.admit(Some(request_id), Some(issued_at)).await, Ok(()));
        tokio::time::sleep(Duration::from_millis(5)).await;

        assert_eq!(guard.admit(Some(request_id), Some(issued_at)).await, Ok(()));
    }
}
