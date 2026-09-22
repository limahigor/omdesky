use rand::RngExt;
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

pub const CHALLENGE_TTL: Duration = Duration::from_secs(120);
pub const MAX_OUTSTANDING_PER_OWNER: usize = 2;
pub const MAX_ISSUES_PER_WINDOW: u32 = 5;
pub const ISSUE_WINDOW: Duration = Duration::from_secs(300);
pub const MAX_TRACKED_OWNERS: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairingError {
    RateLimited,
    UnknownChallenge,
}

impl PairingError {
    pub fn code(self) -> &'static str {
        match self {
            PairingError::RateLimited => "PAIRING_RATE_LIMITED",
            PairingError::UnknownChallenge => "PAIRING_CHALLENGE_INVALID",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            PairingError::RateLimited => {
                "Too many pairing attempts. Wait a few minutes and try again."
            }
            PairingError::UnknownChallenge => {
                "This pairing attempt is no longer valid. Start pairing again."
            }
        }
    }
}

struct Challenge {
    owner: String,
    expires_at: Instant,
}

#[derive(Default)]
struct IssueCounter {
    issues: u32,
    window_started_at: Option<Instant>,
}

pub struct PairingChallenges {
    issued: Mutex<HashMap<String, Challenge>>,
    counters: Mutex<HashMap<String, IssueCounter>>,
    ttl: Duration,
}

impl Default for PairingChallenges {
    fn default() -> Self {
        Self::new(CHALLENGE_TTL)
    }
}

impl PairingChallenges {
    pub fn new(ttl: Duration) -> Self {
        Self {
            issued: Mutex::new(HashMap::new()),
            counters: Mutex::new(HashMap::new()),
            ttl,
        }
    }

    pub fn ttl_seconds(&self) -> u64 {
        self.ttl.as_secs()
    }

    pub async fn issue(&self, owner: &str) -> Result<String, PairingError> {
        let now = Instant::now();

        self.admit_issue(owner, now).await?;

        let mut issued = self.issued.lock().await;
        issued.retain(|_, challenge| challenge.expires_at > now);

        let outstanding = issued
            .values()
            .filter(|challenge| challenge.owner == owner)
            .count();

        if outstanding >= MAX_OUTSTANDING_PER_OWNER {
            return Err(PairingError::RateLimited);
        }

        let pairing_id = random_challenge_id();
        issued.insert(
            pairing_id.clone(),
            Challenge {
                owner: owner.to_owned(),
                expires_at: now + self.ttl,
            },
        );

        Ok(pairing_id)
    }

    pub async fn consume(&self, owner: &str, pairing_id: &str) -> Result<(), PairingError> {
        let now = Instant::now();
        let mut issued = self.issued.lock().await;
        issued.retain(|_, challenge| challenge.expires_at > now);

        match issued.get(pairing_id) {
            Some(challenge) if challenge.owner == owner => {
                issued.remove(pairing_id);

                Ok(())
            }
            _ => Err(PairingError::UnknownChallenge),
        }
    }

    async fn admit_issue(&self, owner: &str, now: Instant) -> Result<(), PairingError> {
        let mut counters = self.counters.lock().await;

        counters.retain(|_, counter| {
            counter
                .window_started_at
                .is_some_and(|started| now.duration_since(started) < ISSUE_WINDOW)
        });

        if counters.len() >= MAX_TRACKED_OWNERS && !counters.contains_key(owner) {
            return Err(PairingError::RateLimited);
        }

        let counter = counters.entry(owner.to_owned()).or_default();
        let window_expired = counter
            .window_started_at
            .is_none_or(|started| now.duration_since(started) >= ISSUE_WINDOW);

        if window_expired {
            counter.window_started_at = Some(now);
            counter.issues = 0;
        }

        if counter.issues >= MAX_ISSUES_PER_WINDOW {
            return Err(PairingError::RateLimited);
        }

        counter.issues += 1;

        Ok(())
    }
}

fn random_challenge_id() -> String {
    let bytes: [u8; 16] = rand::rng().random();

    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_a_challenge_is_single_use() {
        let challenges = PairingChallenges::default();
        let pairing_id = challenges.issue("node-a").await.expect("issued");

        assert_eq!(challenges.consume("node-a", &pairing_id).await, Ok(()));
        assert_eq!(
            challenges.consume("node-a", &pairing_id).await,
            Err(PairingError::UnknownChallenge)
        );
    }

    #[tokio::test]
    async fn test_a_challenge_is_bound_to_the_identity_it_was_issued_to() {
        let challenges = PairingChallenges::default();
        let pairing_id = challenges.issue("node-a").await.expect("issued");

        assert_eq!(
            challenges.consume("node-b", &pairing_id).await,
            Err(PairingError::UnknownChallenge)
        );
        assert_eq!(challenges.consume("node-a", &pairing_id).await, Ok(()));
    }

    #[tokio::test]
    async fn test_an_invented_challenge_is_rejected() {
        let challenges = PairingChallenges::default();

        assert_eq!(
            challenges.consume("node-a", "0123456789abcdef").await,
            Err(PairingError::UnknownChallenge)
        );
    }

    #[tokio::test]
    async fn test_an_expired_challenge_is_rejected() {
        let challenges = PairingChallenges::new(Duration::from_millis(1));
        let pairing_id = challenges.issue("node-a").await.expect("issued");

        tokio::time::sleep(Duration::from_millis(5)).await;

        assert_eq!(
            challenges.consume("node-a", &pairing_id).await,
            Err(PairingError::UnknownChallenge)
        );
    }

    #[tokio::test]
    async fn test_pairing_attempts_are_rate_limited_per_identity() {
        let challenges = PairingChallenges::default();

        for _ in 0..MAX_OUTSTANDING_PER_OWNER {
            challenges.issue("node-a").await.expect("issued");
        }

        assert_eq!(
            challenges.issue("node-a").await,
            Err(PairingError::RateLimited)
        );
    }

    #[tokio::test]
    async fn test_issuing_is_capped_over_the_window_even_after_consumption() {
        let challenges = PairingChallenges::default();

        for _ in 0..MAX_ISSUES_PER_WINDOW {
            let pairing_id = challenges.issue("node-a").await.expect("issued");
            challenges
                .consume("node-a", &pairing_id)
                .await
                .expect("consumed");
        }

        assert_eq!(
            challenges.issue("node-a").await,
            Err(PairingError::RateLimited)
        );
    }

    #[test]
    fn test_challenge_identifiers_are_long_and_unpredictable() {
        let first = random_challenge_id();
        let second = random_challenge_id();

        assert_eq!(first.len(), 32);
        assert_ne!(first, second);
        assert!(first.chars().all(|character| character.is_ascii_hexdigit()));
    }
}
