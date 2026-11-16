/// Default address of the read-only observability listener (`/health`,
/// `/metrics`). Loopback because the endpoints are unauthenticated; operators
/// expose it explicitly when Prometheus scrapes from another host.
pub const DEFAULT_OBSERVABILITY_LISTEN: &str = "127.0.0.1:8870";

/// Maximum number of rate limit buckets kept per rule. Older buckets are
/// evicted (least recently used) once the cap is reached.
pub const DEFAULT_RATELIMIT_MAX_BUCKETS: usize = 100_000;

/// Maximum number of distinct client IPs tracked by the ip_ban middleware.
pub const DEFAULT_IP_BAN_MAX_TRACKED_IPS: usize = 10_000;

pub const TIMER_WAIT_SECONDS: u64 = 5;

/// Upper bound for buffered bodies on the HTTP→gRPC transcoding path.
pub const MAX_TRANSCODE_BODY_BYTES: usize = 16 * 1024 * 1024;

pub const GRPC_STATUS_HEADER: &str = "grpc-status";
pub const GRPC_STATUS_OK: &str = "0";
