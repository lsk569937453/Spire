use serde::{Deserialize, Serialize};

/// Per-route concurrency limit for a route's upstream endpoints.
///
/// The limiter itself is keyed by endpoint (`scheme://host:port`), so several
/// routes pointing at the same endpoint share one limiter; the effective limit
/// is the one configured on the route that first registered the endpoint.
///
/// - `limit`: max in-flight upstream requests. When the semaphore is
///   exhausted the behavior depends on the queue settings.
/// - `queue_length`: max number of *waiters* (memory bound). `0` rejects
///   immediately (fail fast, recommended for a proxy: queued requests still
///   hold the client connection and their buffers).
/// - `queue_timeout_ms`: max time a single request may wait for a permit
///   (latency bound). Only effective when `queue_length > 0`. Waiting longer
///   than this is pointless: the client's own timeout keeps ticking and the
///   request would rather be failed fast so the caller can retry elsewhere.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConcurrencyConfig {
    pub limit: usize,
    #[serde(default)]
    pub queue_length: u32,
    #[serde(default = "default_queue_timeout_ms")]
    pub queue_timeout_ms: u64,
}

fn default_queue_timeout_ms() -> u64 {
    50
}

impl ConcurrencyConfig {
    /// No explicit route-level configuration: fall back to the global
    /// `upstream_concurrency_limit` with no queuing.
    pub fn global_default(limit: usize) -> Self {
        Self {
            limit,
            queue_length: 0,
            queue_timeout_ms: default_queue_timeout_ms(),
        }
    }
}
