use omdesky_application::ports::{AccessStore, AllowedController, MeshNetwork};
use omdesky_core::{ControlCapability, is_tailscale_address};
use std::{
    collections::HashMap,
    net::IpAddr,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, Semaphore};

pub const IDENTITY_CACHE_TTL: Duration = Duration::from_secs(15);
pub const IDENTITY_CACHE_CAPACITY: usize = 512;
pub const ALLOWLIST_CACHE_TTL: Duration = Duration::from_secs(2);
pub const MAX_CONCURRENT_IDENTITY_LOOKUPS: usize = 8;
pub const IDENTITY_LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

pub const RATE_LIMIT_BURST: u32 = 60;
pub const RATE_LIMIT_PER_SECOND: f64 = 5.0;
pub const RATE_LIMIT_TRACKED_SOURCES: usize = 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizedPeer {
    pub tailnet_node_id: String,
    pub capabilities: Vec<ControlCapability>,
}

impl AuthorizedPeer {
    pub fn allows(&self, capability: ControlCapability) -> bool {
        self.capabilities.contains(&capability)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorizationError {
    RateLimited,
    Unavailable,
    Unauthorized,
    Forbidden,
    AccessStoreFailed,
    OutsideTailnet,
}

struct CachedIdentity {
    tailnet_node_id: String,
    resolved_at: Instant,
}

struct CachedAllowlist {
    entries: Vec<AllowedController>,
    read_at: Instant,
}

pub struct Authorizer {
    mesh: Arc<dyn MeshNetwork>,
    access: Arc<dyn AccessStore>,
    identities: Mutex<HashMap<IpAddr, CachedIdentity>>,
    allowlist: Mutex<Option<CachedAllowlist>>,
    lookups: Semaphore,
    limiter: Mutex<RateLimiter>,
    strict_tailnet_only: bool,
}

impl Authorizer {
    pub fn new(mesh: Arc<dyn MeshNetwork>, access: Arc<dyn AccessStore>) -> Self {
        Self::with_strict_tailnet_only(mesh, access, true)
    }

    pub fn with_strict_tailnet_only(
        mesh: Arc<dyn MeshNetwork>,
        access: Arc<dyn AccessStore>,
        strict_tailnet_only: bool,
    ) -> Self {
        Self {
            mesh,
            access,
            identities: Mutex::new(HashMap::new()),
            allowlist: Mutex::new(None),
            lookups: Semaphore::new(MAX_CONCURRENT_IDENTITY_LOOKUPS),
            limiter: Mutex::new(RateLimiter::new(
                RATE_LIMIT_BURST,
                RATE_LIMIT_PER_SECOND,
                RATE_LIMIT_TRACKED_SOURCES,
            )),
            strict_tailnet_only,
        }
    }

    pub async fn authorize(
        &self,
        source: IpAddr,
        required: ControlCapability,
    ) -> Result<AuthorizedPeer, AuthorizationError> {
        if self.strict_tailnet_only && !is_tailscale_address(source) {
            tracing::warn!("authorize.source_outside_tailnet");

            return Err(AuthorizationError::OutsideTailnet);
        }

        if !self.limiter.lock().await.admit(source, Instant::now()) {
            tracing::warn!("authorize.rate_limited");

            return Err(AuthorizationError::RateLimited);
        }

        let tailnet_node_id = self.identify(source).await?;
        let allowlist = self.allowlist().await?;

        let entry = allowlist
            .iter()
            .find(|entry| entry.tailnet_node_id == tailnet_node_id)
            .ok_or(AuthorizationError::Unauthorized)?;

        if !entry.allows(required) {
            tracing::warn!(
                capability = required.as_str(),
                "authorize.capability_denied"
            );

            return Err(AuthorizationError::Forbidden);
        }

        Ok(AuthorizedPeer {
            tailnet_node_id: entry.tailnet_node_id.clone(),
            capabilities: entry.capabilities.clone(),
        })
    }

    async fn identify(&self, source: IpAddr) -> Result<String, AuthorizationError> {
        let now = Instant::now();

        if let Some(cached) = self.identities.lock().await.get(&source)
            && now.duration_since(cached.resolved_at) < IDENTITY_CACHE_TTL
        {
            return Ok(cached.tailnet_node_id.clone());
        }

        let permit = tokio::time::timeout(IDENTITY_LOOKUP_TIMEOUT, self.lookups.acquire())
            .await
            .map_err(|_| AuthorizationError::Unavailable)?
            .map_err(|_| AuthorizationError::Unavailable)?;

        let identity =
            tokio::time::timeout(IDENTITY_LOOKUP_TIMEOUT, self.mesh.identify_source(source))
                .await
                .map_err(|_| AuthorizationError::Unavailable)?
                .map_err(|_| AuthorizationError::Unauthorized)?
                .ok_or(AuthorizationError::Unauthorized)?;

        drop(permit);

        if identity.tailnet_node_id.is_empty() {
            return Err(AuthorizationError::Unauthorized);
        }

        let mut identities = self.identities.lock().await;

        if identities.len() >= IDENTITY_CACHE_CAPACITY {
            identities
                .retain(|_, cached| now.duration_since(cached.resolved_at) < IDENTITY_CACHE_TTL);
        }

        if identities.len() < IDENTITY_CACHE_CAPACITY {
            identities.insert(
                source,
                CachedIdentity {
                    tailnet_node_id: identity.tailnet_node_id.clone(),
                    resolved_at: now,
                },
            );
        }

        Ok(identity.tailnet_node_id)
    }

    async fn allowlist(&self) -> Result<Vec<AllowedController>, AuthorizationError> {
        let now = Instant::now();
        let mut cached = self.allowlist.lock().await;

        if let Some(current) = cached.as_ref()
            && now.duration_since(current.read_at) < ALLOWLIST_CACHE_TTL
        {
            return Ok(current.entries.clone());
        }

        let entries = self.access.list().await.map_err(|error| {
            tracing::warn!(
                code = error.code,
                detail = %error.message,
                "authorize.access_store_failed"
            );

            AuthorizationError::AccessStoreFailed
        })?;

        *cached = Some(CachedAllowlist {
            entries: entries.clone(),
            read_at: now,
        });

        Ok(entries)
    }

    pub async fn invalidate(&self) {
        self.identities.lock().await.clear();
        *self.allowlist.lock().await = None;
    }
}

struct Bucket {
    tokens: f64,
    updated_at: Instant,
}

struct RateLimiter {
    buckets: HashMap<IpAddr, Bucket>,
    burst: f64,
    per_second: f64,
    capacity: usize,
}

impl RateLimiter {
    fn new(burst: u32, per_second: f64, capacity: usize) -> Self {
        Self {
            buckets: HashMap::new(),
            burst: f64::from(burst),
            per_second,
            capacity,
        }
    }

    fn admit(&mut self, source: IpAddr, now: Instant) -> bool {
        if self.buckets.len() >= self.capacity && !self.buckets.contains_key(&source) {
            self.prune(now);
        }

        if self.buckets.len() >= self.capacity && !self.buckets.contains_key(&source) {
            return false;
        }

        let burst = self.burst;
        let per_second = self.per_second;
        let bucket = self.buckets.entry(source).or_insert(Bucket {
            tokens: burst,
            updated_at: now,
        });

        let elapsed = now.duration_since(bucket.updated_at).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * per_second).min(burst);
        bucket.updated_at = now;

        if bucket.tokens < 1.0 {
            return false;
        }

        bucket.tokens -= 1.0;

        true
    }

    fn prune(&mut self, now: Instant) {
        let burst = self.burst;
        let per_second = self.per_second;

        self.buckets.retain(|_, bucket| {
            let elapsed = now.duration_since(bucket.updated_at).as_secs_f64();

            bucket.tokens + elapsed * per_second < burst
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use omdesky_application::ports::{ConnectionInfo, MeshNodeIdentity, PortError, PortResult};
    use omdesky_core::MeshPeer;
    use std::{
        net::Ipv4Addr,
        sync::atomic::{AtomicUsize, Ordering},
    };
    use time::OffsetDateTime;

    struct CountingMesh {
        lookups: AtomicUsize,
        identity: Option<&'static str>,
    }

    #[async_trait]
    impl MeshNetwork for CountingMesh {
        async fn local_node(&self) -> PortResult<MeshNodeIdentity> {
            Err(PortError::new("UNUSED", "unused", false))
        }

        async fn peers(&self) -> PortResult<Vec<MeshPeer>> {
            Ok(Vec::new())
        }

        async fn connection_info(&self, _id: &str) -> PortResult<ConnectionInfo> {
            Err(PortError::new("UNUSED", "unused", false))
        }

        async fn identify_source(&self, _source: IpAddr) -> PortResult<Option<MeshNodeIdentity>> {
            self.lookups.fetch_add(1, Ordering::SeqCst);

            Ok(self.identity.map(|id| MeshNodeIdentity {
                tailnet_node_id: id.to_owned(),
                user: None,
                hostname: None,
                addresses: Vec::new(),
            }))
        }
    }

    struct StaticAccess {
        entries: Vec<AllowedController>,
    }

    #[async_trait]
    impl AccessStore for StaticAccess {
        async fn list(&self) -> PortResult<Vec<AllowedController>> {
            Ok(self.entries.clone())
        }

        async fn allow(&self, _controller: AllowedController) -> PortResult<()> {
            Ok(())
        }

        async fn revoke(&self, _tailnet_node_id: &str) -> PortResult<()> {
            Ok(())
        }

        async fn is_allowed(&self, _tailnet_node_id: &str) -> PortResult<bool> {
            Ok(false)
        }
    }

    fn source() -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(100, 64, 0, 7))
    }

    fn authorizer(
        identity: Option<&'static str>,
        entries: Vec<AllowedController>,
    ) -> (Authorizer, Arc<CountingMesh>) {
        let mesh = Arc::new(CountingMesh {
            lookups: AtomicUsize::new(0),
            identity,
        });

        (
            Authorizer::new(mesh.clone(), Arc::new(StaticAccess { entries })),
            mesh,
        )
    }

    fn entry(id: &str, capabilities: &[ControlCapability]) -> AllowedController {
        AllowedController::new(
            id,
            None,
            OffsetDateTime::UNIX_EPOCH,
            capabilities.iter().copied(),
        )
    }

    #[tokio::test]
    async fn test_a_source_outside_the_tailnet_is_denied_before_any_lookup() {
        let (authorizer, mesh) = authorizer(
            Some("node-a"),
            vec![entry("node-a", &ControlCapability::ALL)],
        );

        let error = authorizer
            .authorize(
                IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10)),
                ControlCapability::ReadMetadata,
            )
            .await
            .expect_err("a non-tailnet source is denied");

        assert_eq!(error, AuthorizationError::OutsideTailnet);
        assert_eq!(mesh.lookups.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn test_relaxing_the_tailnet_check_allows_other_sources() {
        let mesh = Arc::new(CountingMesh {
            lookups: AtomicUsize::new(0),
            identity: Some("node-a"),
        });
        let authorizer = Authorizer::with_strict_tailnet_only(
            mesh,
            Arc::new(StaticAccess {
                entries: vec![entry("node-a", &ControlCapability::ALL)],
            }),
            false,
        );

        assert!(
            authorizer
                .authorize(
                    IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10)),
                    ControlCapability::ReadMetadata,
                )
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn test_an_empty_allowlist_denies_every_control_action() {
        let (authorizer, _) = authorizer(Some("node-a"), Vec::new());

        let error = authorizer
            .authorize(source(), ControlCapability::ReadMetadata)
            .await
            .expect_err("an empty allowlist authorizes nobody");

        assert_eq!(error, AuthorizationError::Unauthorized);
    }

    #[tokio::test]
    async fn test_an_unlisted_identity_is_denied() {
        let (authorizer, _) = authorizer(
            Some("node-b"),
            vec![entry("node-a", &ControlCapability::ALL)],
        );

        assert_eq!(
            authorizer
                .authorize(source(), ControlCapability::ReadMetadata)
                .await
                .expect_err("an unlisted identity is denied"),
            AuthorizationError::Unauthorized
        );
    }

    #[tokio::test]
    async fn test_a_listed_identity_is_limited_to_its_granted_capabilities() {
        let (authorizer, _) = authorizer(
            Some("node-a"),
            vec![entry("node-a", &[ControlCapability::ReadMetadata])],
        );

        let peer = authorizer
            .authorize(source(), ControlCapability::ReadMetadata)
            .await
            .expect("reading is granted");
        assert_eq!(peer.tailnet_node_id, "node-a");

        assert_eq!(
            authorizer
                .authorize(source(), ControlCapability::ApprovePairing)
                .await
                .expect_err("pairing is not granted"),
            AuthorizationError::Forbidden
        );
    }

    #[tokio::test]
    async fn test_an_unresolvable_source_is_denied() {
        let (authorizer, _) = authorizer(None, vec![entry("node-a", &ControlCapability::ALL)]);

        assert_eq!(
            authorizer
                .authorize(source(), ControlCapability::ReadMetadata)
                .await
                .expect_err("an unknown source is denied"),
            AuthorizationError::Unauthorized
        );
    }

    #[tokio::test]
    async fn test_repeated_requests_reuse_one_identity_lookup() {
        let (authorizer, mesh) = authorizer(
            Some("node-a"),
            vec![entry("node-a", &ControlCapability::ALL)],
        );

        for _ in 0..10 {
            authorizer
                .authorize(source(), ControlCapability::ReadMetadata)
                .await
                .expect("authorized");
        }

        assert_eq!(mesh.lookups.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_a_flood_from_one_source_is_rate_limited() {
        let (authorizer, _) = authorizer(
            Some("node-a"),
            vec![entry("node-a", &ControlCapability::ALL)],
        );

        let mut denials = 0;

        for _ in 0..(RATE_LIMIT_BURST + 20) {
            if authorizer
                .authorize(source(), ControlCapability::ReadMetadata)
                .await
                .is_err()
            {
                denials += 1;
            }
        }

        assert!(denials > 0);
    }

    #[test]
    fn test_the_rate_limiter_refills_over_time() {
        let mut limiter = RateLimiter::new(2, 1.0, 8);
        let start = Instant::now();

        assert!(limiter.admit(source(), start));
        assert!(limiter.admit(source(), start));
        assert!(!limiter.admit(source(), start));
        assert!(limiter.admit(source(), start + Duration::from_secs(1)));
    }

    #[test]
    fn test_the_rate_limiter_bounds_the_sources_it_tracks() {
        let mut limiter = RateLimiter::new(1, 0.0, 2);
        let now = Instant::now();

        assert!(limiter.admit(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1)), now));
        assert!(limiter.admit(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 2)), now));
        assert!(!limiter.admit(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 3)), now));
    }
}
