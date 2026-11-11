//! Per-endpoint concurrency limiting (backpressure).
//!
//! The async runtime does not consume a thread per in-flight request, so a
//! slow downstream no longer stalls other routes — but it also removes the
//! natural bound a synchronous thread pool provided: in-flight requests grow
//! without limit until the process runs out of memory, and the extra
//! concurrent load makes an already-slow downstream slower. This module
//! rebuilds that bound explicitly, scoped per endpoint (`scheme://host:port`)
//! so one slow downstream can never eat another endpoint's budget.
//!
//! Semantics (see `ConcurrencyConfig`):
//! - `queue_length == 0`: fail fast. `try_acquire`, reject with 503 on
//!   exhaustion. Queued requests still hold the client connection and their
//!   buffers, so unbounded queuing just hides the failure and burns memory.
//! - `queue_length > 0`: bounded queuing — at most `queue_length` waiters
//!   (memory bound), each waiting at most `queue_timeout_ms` (latency bound).
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
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::OwnedSemaphorePermit;
use tokio::sync::Semaphore;
use url::Url;

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
}

impl AcquireError {
    pub fn reason(&self) -> &'static str {
        match self {
            AcquireError::Rejected => "upstream concurrency limit exhausted",
            AcquireError::TimedOut => "upstream concurrency queue wait timed out",
        }
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

/// Process-wide registry of endpoint limiters. Entries survive config
/// reloads; re-registering with a different config replaces the limiter so
/// in-flight permits held by the old semaphore drain naturally.
pub struct ConcurrencyRegistry {
    limiters: RwLock<HashMap<String, Arc<UpstreamLimiter>>>,
}

static REGISTRY: OnceLock<ConcurrencyRegistry> = OnceLock::new();

fn registry() -> &'static ConcurrencyRegistry {
    REGISTRY.get_or_init(|| ConcurrencyRegistry {
        limiters: RwLock::new(HashMap::new()),
    })
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

/// Resolves (or creates) the limiter for `endpoint` and acquires a permit.
///
/// On failure the rejection is counted and `Err` is returned; the caller
/// answers with 503 + `Retry-After` *before* touching the upstream, on the
/// early-return path that bypasses `record_outcome` — the breaker must only
/// ever judge the downstream's health, not our own backpressure.
pub async fn acquire(
    endpoint: &str,
    cfg: &ConcurrencyConfig,
    mapping_key: &str,
    route: &str,
) -> Result<OwnedSemaphorePermit, AppError> {
    let key = endpoint_key(endpoint);
    let limiter = limiter_for(&key, cfg);
    metrics::set_upstream_concurrency_limit(
        mapping_key,
        route,
        &key,
        cfg.limit as i64,
    );
    match limiter.acquire_owned().await {
        Ok(permit) => Ok(permit),
        Err(e) => {
            debug!(
                "[ConcurrencyLimit] {e:?} for endpoint {key} (limit {}, route {route})",
                cfg.limit
            );
            metrics::record_concurrency_rejected(mapping_key, route, &key);
            Err(AppError(format!("{}: {}", e.reason(), key)))
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
        assert_eq!(endpoint_key("https://api.example.com"), "https://api.example.com");
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
        let _held = limiter.acquire_owned().await;
        let waiter = limiter.clone();
        let task = tokio::spawn(async move { waiter.acquire_owned().await });
        // Give the waiter time to park, then release the permit.
        tokio::time::sleep(Duration::from_millis(20)).await;
        drop(_held);
        assert!(task.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn bounded_waiter_queue_rejects_beyond_queue_length() {
        let limiter = Arc::new(UpstreamLimiter::new(cfg(1, 1, 5_000)));
        let held = limiter.acquire_owned().await.unwrap();
        let waiter_limiter = limiter.clone();
        // Park one waiter so it occupies the single queue slot.
        let parked =
            tokio::spawn(async move { waiter_limiter.acquire_owned().await });
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
}
