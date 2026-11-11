//! Two-layer backpressure: per-endpoint shaping plus a process-wide memory
//! guard.
//!
//! The async runtime does not consume a thread per in-flight request, so a
//! slow downstream no longer stalls other routes — but it also removes the
//! natural bound a synchronous thread pool provided. This module rebuilds
//! that bound explicitly, in two layers:
//!
//! 1. **Per-endpoint semaphores** (scoped to `scheme://host:port`): traffic
//!    shaping and isolation. One slow downstream can never eat another
//!    endpoint's budget. Queuing semantics come from `ConcurrencyConfig`.
//! 2. **Global memory guard** (one process-wide semaphore): the actual OOM
//!    backstop. Endpoint limits alone cannot bound total memory when several
//!    endpoints degrade at once, so every in-flight request also takes
//!    (weighted) permits from a global budget derived at startup from the
//!    container memory limit:
//!
//!    ```text
//!    global permits = memory_limit × MEMORY_BUDGET_FRACTION ÷ PER_REQUEST_ESTIMATE_BYTES
//!    endpoint default (when unconfigured) = min(10000, global ÷ MAX_ENDPOINT_SHARE)
//!    ```
//!
//!    An explicit `upstream_concurrency_limit` or route `concurrency.limit`
//!    always wins over the derived defaults. The global guard never queues:
//!    when the memory budget is exhausted, waiting would itself cost memory,
//!    so requests fail fast.
//!
//! A permit must be held from just before the upstream request is issued
//! until the response body has fully streamed (see `PermitBody`); limiting
//! only until response headers arrive would bound almost nothing.

use crate::monitor::prometheus_exporter::metrics;
use crate::vojo::app_error::AppError;
use crate::vojo::concurrency::ConcurrencyConfig;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::RwLock;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::OwnedSemaphorePermit;
use tokio::sync::Semaphore;
use url::Url;

/// Share of the detected memory limit reserved for in-flight request state.
/// The other half covers page cache (cgroup accounting counts it), the
/// process baseline, allocator fragmentation and headroom.
pub const MEMORY_BUDGET_FRACTION: f64 = 0.5;

/// Estimated footprint of one streamed HTTP request in flight (async task +
/// hyper buffers + headers + socket state). gRPC transcode requests are
/// weighted by their actual body size instead of assuming this constant.
pub const PER_REQUEST_ESTIMATE_BYTES: usize = 64 * 1024;

/// When a route relies on the derived default, a single endpoint may claim
/// at most this share of the global budget — the rest stays available for
/// every other endpoint (healthy traffic needs only single-digit permits).
pub const MAX_ENDPOINT_SHARE: usize = 4;

/// Fallbacks when no memory limit could be detected (e.g. Windows dev
/// machines): identical to the previous hardcoded behavior.
pub const FALLBACK_ENDPOINT_DEFAULT: usize = 10_000;
pub const FALLBACK_GLOBAL_PERMITS: usize = 40_000;

/// Result of the startup derivation, for logging and metrics.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DerivedLimitsInfo {
    pub memory_limit_bytes: u64,
    pub global_permits: usize,
    pub endpoint_default: usize,
    pub budget_fraction: f64,
    pub per_request_estimate_bytes: usize,
}

/// Normalizes a configured endpoint URL to `scheme://host[:port]`.
///
/// The path is dropped on purpose: every route variant that differs only in
/// path prefixes hits the same downstream and must share one limiter. Parse
/// failures fall back to the raw string — the key stays bounded because
/// endpoints come from configuration, never from the request.
pub fn endpoint_key(endpoint: &str) -> String {
    match Url::parse(endpoint) {
        Ok(url) if url.has_host() => {
            let mut key = format!("{}://{}", url.scheme(), url.host_str().unwrap_or_default());
            if let Some(port) = url.port() {
                key.push_str(&format!(":{port}"));
            }
            key
        }
        _ => endpoint.to_string(),
    }
}

/// A permit could not be acquired for this request.
#[derive(Debug, PartialEq, Eq)]
pub enum AcquireError {
    /// No free permit and queuing is disabled (or the waiter queue is full).
    Rejected,
    /// Queued but the `queue_timeout_ms` budget elapsed before a permit
    /// became available.
    TimedOut,
    /// The process-wide memory budget is exhausted: the last-resort OOM
    /// protection is engaging.
    GlobalBudgetExhausted,
}

impl AcquireError {
    pub fn reason(&self) -> &'static str {
        match self {
            AcquireError::Rejected => "upstream concurrency limit exhausted",
            AcquireError::TimedOut => "upstream concurrency queue wait timed out",
            AcquireError::GlobalBudgetExhausted => {
                "global concurrency memory budget exhausted"
            }
        }
    }
}

/// The pair of permits one in-flight upstream request holds: one from its
/// endpoint limiter (isolation) and weighted permits from the global memory
/// guard (OOM backstop). Both are released when the value drops — i.e. when
/// `PermitBody` finishes streaming the response body.
pub struct UpstreamPermit {
    // Merely holding these permits IS the mechanism: each is released when
    // this guard drops (end of the response body stream). They are never
    // read otherwise.
    #[allow(dead_code)]
    endpoint: OwnedSemaphorePermit,
    #[allow(dead_code)]
    global: OwnedSemaphorePermit,
}

impl UpstreamPermit {
    pub(crate) fn from_parts(endpoint: OwnedSemaphorePermit, global: OwnedSemaphorePermit) -> Self {
        Self { endpoint, global }
    }
}

// The inner permits are not Debug; the pair itself never needs a payload.
impl std::fmt::Debug for UpstreamPermit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpstreamPermit").finish()
    }
}

pub struct UpstreamLimiter {
    permits: Arc<Semaphore>,
    /// Tasks currently parked waiting for a permit; only consulted when
    /// `queue_length > 0`. This is the memory bound on queuing.
    waiters: std::sync::atomic::AtomicU32,
    cfg: ConcurrencyConfig,
}

impl UpstreamLimiter {
    fn new(cfg: ConcurrencyConfig) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(cfg.limit)),
            waiters: std::sync::atomic::AtomicU32::new(0),
            cfg,
        }
    }

    async fn acquire_owned(&self) -> Result<OwnedSemaphorePermit, AcquireError> {
        if self.cfg.queue_length == 0 {
            return self
                .permits
                .clone()
                .try_acquire_owned()
                .map_err(|_| AcquireError::Rejected);
        }
        if self.waiters.load(Ordering::Relaxed) >= self.cfg.queue_length {
            return Err(AcquireError::Rejected);
        }
        self.waiters.fetch_add(1, Ordering::Relaxed);
        let wait = tokio::time::timeout(
            Duration::from_millis(self.cfg.queue_timeout_ms),
            self.acquire_owned_slow(),
        )
        .await;
        self.waiters.fetch_sub(1, Ordering::Relaxed);
        match wait {
            // A closed semaphore means the limiter was replaced by a reload;
            // rejecting is safe, the next request picks up the new limiter.
            Ok(Ok(permit)) => Ok(permit),
            Ok(Err(_)) | Err(_) => Err(AcquireError::TimedOut),
        }
    }

    async fn acquire_owned_slow(&self) -> Result<OwnedSemaphorePermit, tokio::sync::AcquireError> {
        self.permits.clone().acquire_owned().await
    }
}

/// Process-wide registry of endpoint limiters plus the global memory guard.
/// Entries survive config reloads; re-registering with a different config
/// replaces the limiter so in-flight permits held by the old semaphore drain
/// naturally. The same drain semantics apply to a re-derived global guard.
pub struct ConcurrencyRegistry {
    limiters: RwLock<HashMap<String, Arc<UpstreamLimiter>>>,
    /// The OOM backstop, sized from the detected memory limit at startup.
    global: RwLock<Arc<Semaphore>>,
    /// Derived per-endpoint default used when neither the route nor the
    /// global config sets an explicit limit.
    endpoint_default: AtomicUsize,
}

static REGISTRY: OnceLock<ConcurrencyRegistry> = OnceLock::new();

fn registry() -> &'static ConcurrencyRegistry {
    REGISTRY.get_or_init(|| ConcurrencyRegistry {
        limiters: RwLock::new(HashMap::new()),
        global: RwLock::new(Arc::new(Semaphore::new(FALLBACK_GLOBAL_PERMITS))),
        endpoint_default: AtomicUsize::new(FALLBACK_ENDPOINT_DEFAULT),
    })
}

/// Pure derivation of the two permit numbers — separated from the registry
/// so the math is directly testable.
pub fn derive_permits(memory_limit_bytes: u64) -> (usize, usize) {
    let budget = (memory_limit_bytes as f64 * MEMORY_BUDGET_FRACTION) as u64;
    let global = ((budget / PER_REQUEST_ESTIMATE_BYTES as u64) as usize).max(1);
    let endpoint = FALLBACK_ENDPOINT_DEFAULT
        .min(global / MAX_ENDPOINT_SHARE)
        .max(1);
    (global, endpoint)
}

/// Sizes the global memory guard from the detected memory limit. Called once
/// at startup; safe to call again (a re-derivation replaces the guard, and
/// in-flight permits of the old one drain naturally).
pub fn init_derived_limits(memory_limit_bytes: Option<u64>) -> DerivedLimitsInfo {
    let (global_permits, endpoint_default) = match memory_limit_bytes {
        Some(bytes) => derive_permits(bytes),
        None => (FALLBACK_GLOBAL_PERMITS, FALLBACK_ENDPOINT_DEFAULT),
    };
    let info = DerivedLimitsInfo {
        memory_limit_bytes: memory_limit_bytes.unwrap_or(0),
        global_permits,
        endpoint_default,
        budget_fraction: MEMORY_BUDGET_FRACTION,
        per_request_estimate_bytes: PER_REQUEST_ESTIMATE_BYTES,
    };
    if let Ok(mut global) = registry().global.write() {
        *global = Arc::new(Semaphore::new(global_permits));
    }
    registry()
        .endpoint_default
        .store(endpoint_default, Ordering::Relaxed);
    info
}

/// Per-endpoint fallback for routes without an explicit concurrency config.
/// Returns the conservative legacy default when no memory limit was
/// detected.
pub fn endpoint_default() -> usize {
    registry().endpoint_default.load(Ordering::Relaxed)
}

fn limiter_for(key: &str, cfg: &ConcurrencyConfig) -> Arc<UpstreamLimiter> {
    if let Ok(map) = registry().limiters.read()
        && let Some(existing) = map.get(key)
        && &existing.cfg == cfg
    {
        return existing.clone();
    }
    let mut map = registry().limiters.write().expect("limiter registry lock poisoned");
    // Another thread may have won the write race while we upgraded the lock.
    if let Some(existing) = map.get(key)
        && &existing.cfg == cfg
    {
        return existing.clone();
    }
    let limiter = Arc::new(UpstreamLimiter::new(cfg.clone()));
    map.insert(key.to_string(), limiter.clone());
    limiter
}

/// Acquires one in-flight slot: an endpoint permit plus one (unweighted)
/// global permit. See `acquire_weighted` for gRPC transcode requests.
pub async fn acquire(
    endpoint: &str,
    cfg: &ConcurrencyConfig,
    mapping_key: &str,
    route: &str,
) -> Result<UpstreamPermit, AppError> {
    acquire_weighted(endpoint, cfg, 1, mapping_key, route).await
}

/// Acquires one in-flight slot with a memory `weight` (in global permits).
///
/// On failure the rejection is counted and `Err` is returned; the caller
/// answers with 503 + `Retry-After` *before* touching the upstream, on the
/// early-return path that bypasses `record_outcome` — the breaker must only
/// ever judge the downstream's health, not our own backpressure.
///
/// Acquisition order is always endpoint → global (no deadlock); the global
/// guard never queues because waiting for memory would itself cost memory.
pub async fn acquire_weighted(
    endpoint: &str,
    cfg: &ConcurrencyConfig,
    weight: usize,
    mapping_key: &str,
    route: &str,
) -> Result<UpstreamPermit, AppError> {
    let key = endpoint_key(endpoint);
    let limiter = limiter_for(&key, cfg);
    metrics::set_upstream_concurrency_limit(mapping_key, route, &key, cfg.limit as i64);
    let endpoint_permit = match limiter.acquire_owned().await {
        Ok(permit) => permit,
        Err(e) => {
            debug!(
                "[ConcurrencyLimit] {e:?} for endpoint {key} (limit {}, route {route})",
                cfg.limit
            );
            metrics::record_concurrency_rejected(mapping_key, route, &key);
            return Err(AppError(format!("{}: {}", e.reason(), key)));
        }
    };
    // Global memory guard: fail fast, weighted by the request's estimated
    // footprint. `try_acquire_many_owned` is atomic — we get all `weight`
    // permits or none.
    let weight = weight.max(1) as u32;
    let global = registry()
        .global
        .read()
        .expect("global guard lock poisoned")
        .clone();
    match global.clone().try_acquire_many_owned(weight) {
        Ok(global_permit) => Ok(UpstreamPermit::from_parts(endpoint_permit, global_permit)),
        Err(_) => {
            // Our own load shedding: undo the endpoint permit and reject
            // before touching the upstream.
            drop(endpoint_permit);
            warn!(
                "[ConcurrencyLimit] global memory budget exhausted (weight {weight}, endpoint {key}, route {route})"
            );
            metrics::record_concurrency_rejected(mapping_key, route, &key);
            Err(AppError(format!("{}: {}", AcquireError::GlobalBudgetExhausted.reason(), key)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(limit: usize, queue_length: u32, queue_timeout_ms: u64) -> ConcurrencyConfig {
        ConcurrencyConfig {
            limit,
            queue_length,
            queue_timeout_ms,
        }
    }

    #[test]
    fn endpoint_key_strips_path_and_keeps_authority() {
        assert_eq!(
            endpoint_key("http://127.0.0.1:9394/some/prefix"),
            "http://127.0.0.1:9394"
        );
        assert_eq!(
            endpoint_key("https://api.example.com"),
            "https://api.example.com"
        );
        assert_eq!(endpoint_key("grpc://10.0.0.1:50051"), "grpc://10.0.0.1:50051");
        // Unparseable endpoints fall back to the raw string, still bounded.
        assert_eq!(endpoint_key("not a url"), "not a url");
    }

    #[tokio::test]
    async fn fail_fast_when_queue_disabled() {
        let limiter = UpstreamLimiter::new(cfg(1, 0, 50));
        let first = limiter.acquire_owned().await;
        assert!(first.is_ok());
        let second = limiter.acquire_owned().await.unwrap_err();
        assert_eq!(second, AcquireError::Rejected);
    }

    #[tokio::test]
    async fn queued_waiter_times_out() {
        // One permit taken, one waiter allowed, tiny wait budget.
        let limiter = UpstreamLimiter::new(cfg(1, 1, 30));
        let _held = limiter.acquire_owned().await;
        let started = std::time::Instant::now();
        let result = limiter.acquire_owned().await.unwrap_err();
        assert_eq!(result, AcquireError::TimedOut);
        assert!(started.elapsed() >= Duration::from_millis(30));
    }

    #[tokio::test]
    async fn queued_waiter_succeeds_when_permit_frees() {
        let limiter = Arc::new(UpstreamLimiter::new(cfg(1, 1, 5_000)));
        let held = limiter.acquire_owned().await.unwrap();
        let waiter = limiter.clone();
        let task = tokio::spawn(async move { waiter.acquire_owned().await });
        // Give the waiter time to park, then release the permit.
        tokio::time::sleep(Duration::from_millis(20)).await;
        drop(held);
        assert!(task.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn bounded_waiter_queue_rejects_beyond_queue_length() {
        let limiter = Arc::new(UpstreamLimiter::new(cfg(1, 1, 5_000)));
        let held = limiter.acquire_owned().await.unwrap();
        let waiter_limiter = limiter.clone();
        // Park one waiter so it occupies the single queue slot.
        let parked = tokio::spawn(async move { waiter_limiter.acquire_owned().await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        // Third caller: permit busy and waiter queue full.
        let third = limiter.acquire_owned().await.unwrap_err();
        assert_eq!(third, AcquireError::Rejected);
        drop(held);
        // The parked waiter takes the freed permit.
        assert!(parked.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn registry_replaces_limiter_when_config_changes() {
        let key = "test://replace:1";
        let a = limiter_for(key, &cfg(1, 0, 50));
        // Same config → same limiter instance.
        let a2 = limiter_for(key, &cfg(1, 0, 50));
        assert!(Arc::ptr_eq(&a, &a2));
        // Different limit → fresh limiter.
        let b = limiter_for(key, &cfg(2, 0, 50));
        assert!(!Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn derive_permits_from_pod_memory() {
        // 4 GiB pod: budget 2 GiB ÷ 64 KiB = 32768 global permits;
        // endpoint default min(10000, 32768/4) = 8192.
        assert_eq!(derive_permits(4 * 1024 * 1024 * 1024), (32768, 8192));
        // 1 GiB pod: 8192 global, min(10000, 2048) = 2048 per endpoint.
        assert_eq!(derive_permits(1024 * 1024 * 1024), (8192, 2048));
        // Degenerate tiny limit still yields usable numbers (never 0).
        assert_eq!(derive_permits(1), (1, 1));
    }

    #[test]
    fn derive_permits_fallback_without_detection() {
        let info = init_derived_limits(None);
        assert_eq!(info.global_permits, FALLBACK_GLOBAL_PERMITS);
        assert_eq!(info.endpoint_default, FALLBACK_ENDPOINT_DEFAULT);
    }

    // These two tests re-derive the process-wide global guard; they must not
    // overlap with other tests whose acquires rely on the default budget.
    #[serial_test::serial]
    #[tokio::test]
    async fn global_guard_rejects_and_releases_endpoint_permit() {
        // Tiny global budget for this test; restored on the way out.
        let previous = init_derived_limits(Some(1));
        let key = format!("test://global-guard:{}", line!());
        let route_cfg = cfg(2, 0, 50);

        // First request takes the only global permit.
        let held = acquire_weighted(&key, &route_cfg, 1, "test", "global-route")
            .await
            .expect("first acquire must succeed");

        // Second request: endpoint has a free permit, but the global memory
        // budget is exhausted → rejected, and its endpoint permit must be
        // released again (otherwise the third acquire below fails).
        let second = acquire_weighted(&key, &route_cfg, 1, "test", "global-route")
            .await
            .unwrap_err();
        assert!(second.0.contains("global concurrency memory budget"));

        drop(held);
        let third = acquire_weighted(&key, &route_cfg, 1, "test", "global-route").await;
        assert!(
            third.is_ok(),
            "endpoint permit must have been released on global rejection"
        );

        // Restore the conservative budget for any concurrently running test.
        init_derived_limits(Some(previous.memory_limit_bytes.max(1)));
        init_derived_limits(None);
    }

    #[serial_test::serial]
    #[tokio::test]
    async fn weighted_acquire_charges_memory_budget() {
        // 1 MiB memory → budget 512 KiB ÷ 64 KiB = 8 global permits.
        let _ = init_derived_limits(Some(1024 * 1024));
        let key = format!("test://weighted:{}", line!());
        let route_cfg = cfg(100, 0, 50);

        // A weight-8 request consumes the whole memory budget...
        let held = acquire_weighted(&key, &route_cfg, 8, "test", "weighted-route")
            .await
            .expect("weight-8 acquire must succeed");
        // ...so even a weight-1 request is now rejected.
        let rejected = acquire_weighted(&key, &route_cfg, 1, "test", "weighted-route").await;
        assert!(rejected.is_err());

        drop(held);
        // After release, weight fits again.
        assert!(acquire_weighted(&key, &route_cfg, 1, "test", "weighted-route")
            .await
            .is_ok());

        // Restore the conservative budget for any concurrently running test.
        init_derived_limits(None);
    }
}
