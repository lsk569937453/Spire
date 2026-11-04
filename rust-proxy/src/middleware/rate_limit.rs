use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::constants::common_constants::DEFAULT_RATELIMIT_MAX_BUCKETS;
use crate::middleware::middlewares::CheckResult;
use crate::middleware::middlewares::Denial;
use crate::middleware::middlewares::Middleware;
use crate::vojo::app_error::AppError;
use core::fmt::Debug;
use http::HeaderMap;
use http::HeaderName;
use http::HeaderValue;
use http::StatusCode;
use http::header;
use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;

const X_RATELIMIT_LIMIT: HeaderName = HeaderName::from_static("x-ratelimit-limit");
const X_RATELIMIT_REMAINING: HeaderName = HeaderName::from_static("x-ratelimit-remaining");
const X_RATELIMIT_RESET: HeaderName = HeaderName::from_static("x-ratelimit-reset");

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "limiter", rename_all = "PascalCase")]
pub enum Ratelimit {
    #[serde(rename = "token_bucket")]
    TokenBucket(TokenBucketRateLimit),
    #[serde(rename = "fixed_window")]
    FixedWindow(FixedWindowRateLimit),
}

impl Middleware for Arc<Mutex<Ratelimit>> {
    fn check_request(
        &self,
        peer_addr: &SocketAddr,
        headers_option: Option<&HeaderMap<HeaderValue>>,
    ) -> Result<CheckResult, AppError> {
        let mut lock = self.lock()?;
        let limit_res = lock.should_limit(headers_option, peer_addr)?;
        if let Some(rate_limit_headers) = limit_res {
            let denial = Denial {
                status: StatusCode::TOO_MANY_REQUESTS,
                headers: rate_limit_headers,
                body: "API rate limit exceeded".to_string(),
            };
            return Ok(CheckResult::Denied(denial));
        }
        Ok(CheckResult::Allowed)
    }
}

impl Ratelimit {
    pub fn should_limit(
        &mut self,
        headers: Option<&HeaderMap<HeaderValue>>,
        peer_addr: &SocketAddr,
    ) -> Result<Option<HeaderMap>, AppError> {
        match self {
            Ratelimit::TokenBucket(tb) => tb.should_limit(headers, peer_addr),
            Ratelimit::FixedWindow(fw) => fw.should_limit(headers, peer_addr),
        }
    }
}

/// Kept for backwards compatibility with existing configuration files: the
/// `value` field of an `IP` scope is ignored, limiting is always per client IP.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct IPBasedRatelimit {
    #[serde(default)]
    pub value: String,
}

/// Limits per distinct value of the configured request header (for example one
/// bucket per API key). When `value` is set, the header must match it exactly
/// for the request to be counted; leave it empty to bucket every distinct value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeaderBasedRatelimit {
    pub key: String,
    #[serde(default)]
    pub value: String,
}

/// Limits every client inside the given CIDR to a single shared bucket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpRangeBasedRatelimit {
    pub value: String,
    /// Parsed form of `value`; parsing a CIDR per request is far too expensive.
    #[serde(skip)]
    parsed: OnceLock<Option<IpNet>>,
}

impl PartialEq for IpRangeBasedRatelimit {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl IpRangeBasedRatelimit {
    fn net(&self) -> Option<&IpNet> {
        self.parsed
            .get_or_init(|| match self.value.parse::<IpNet>() {
                Ok(net) => Some(net),
                Err(e) => {
                    error!("Invalid rate limit address range '{}': {e}", self.value);
                    None
                }
            })
            .as_ref()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum LimitLocation {
    IP(IPBasedRatelimit),
    Header(HeaderBasedRatelimit),
    Iprange(IpRangeBasedRatelimit),
}

impl Default for LimitLocation {
    fn default() -> Self {
        LimitLocation::IP(IPBasedRatelimit::default())
    }
}

impl LimitLocation {
    /// Builds the bucket key for this request.
    ///
    /// Returns `None` when the request is outside the scope of this rule (for
    /// example a header-scoped rule for a request without that header), which
    /// means "do not limit this request".
    fn bucket_key(
        &self,
        headers: Option<&HeaderMap<HeaderValue>>,
        peer_addr: &SocketAddr,
    ) -> Option<String> {
        match self {
            // The old implementation compared `value` against the client IP,
            // which only ever limited the single hard-coded address from the
            // config. Every rule is now keyed by the real client address.
            LimitLocation::IP(_) => Some(format!("ip:{}", peer_addr.ip())),
            LimitLocation::Header(header_limit) => {
                let raw_value = headers?.get(header_limit.key.as_str())?.to_str().ok()?;
                if !header_limit.value.is_empty() && header_limit.value != raw_value {
                    return None;
                }
                Some(format!("hdr:{}:{}", header_limit.key, raw_value))
            }
            LimitLocation::Iprange(range) => {
                let net = range.net()?;
                if net.contains(&peer_addr.ip()) {
                    Some(format!("net:{}", range.value))
                } else {
                    None
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
#[derive(Default)]
pub enum TimeUnit {
    #[default]
    MillionSecond,
    Second,
    Minute,
    Hour,
    Day,
}

impl TimeUnit {
    fn window_millis(&self) -> u64 {
        match self {
            Self::MillionSecond => 1,
            Self::Second => 1_000,
            Self::Minute => 60_000,
            Self::Hour => 3_600_000,
            Self::Day => 86_400_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TokenBucketRateLimit {
    pub rate_per_unit: i32,
    pub unit: TimeUnit,
    pub capacity: i32,
    pub scope: LimitLocation,
    /// One bucket per scope key (normally per client IP).
    #[serde(skip)]
    pub(crate) buckets: HashMap<String, Bucket>,
}

impl TokenBucketRateLimit {
    fn refill_per_millis(&self) -> f64 {
        self.rate_per_unit as f64 / self.unit.window_millis() as f64
    }

    fn evict_if_needed(&mut self) {
        if self.buckets.len() <= DEFAULT_RATELIMIT_MAX_BUCKETS {
            return;
        }
        // Drop the least recently used bucket. This only runs when the map is
        // over the cap, so the O(n) scan is not on the hot path.
        if let Some(oldest) = self
            .buckets
            .iter()
            .min_by_key(|(_, bucket)| bucket.last_refill)
            .map(|(key, _)| key.clone())
        {
            self.buckets.remove(&oldest);
        }
    }

    fn should_limit(
        &mut self,
        headers: Option<&HeaderMap<HeaderValue>>,
        peer_addr: &SocketAddr,
    ) -> Result<Option<HeaderMap>, AppError> {
        let Some(key) = self.scope.bucket_key(headers, peer_addr) else {
            return Ok(None);
        };

        // A misconfigured limiter must not take the whole route down.
        if self.rate_per_unit <= 0 || self.capacity <= 0 {
            debug!(
                "rate_limit misconfigured (rate_per_unit={}, capacity={}); request is not limited",
                self.rate_per_unit, self.capacity
            );
            return Ok(None);
        }

        let capacity = self.capacity as f64;
        let refill_per_millis = self.refill_per_millis();
        let now = Instant::now();

        let bucket = self.buckets.entry(key).or_insert(Bucket {
            // Start full: a freshly started gateway (or a new client) may burst
            // up to `capacity` requests instead of being rejected immediately.
            tokens: capacity,
            last_refill: now,
        });

        let elapsed_millis = now
            .saturating_duration_since(bucket.last_refill)
            .as_millis() as f64;
        if elapsed_millis > 0.0 {
            bucket.tokens = (bucket.tokens + elapsed_millis * refill_per_millis).min(capacity);
            bucket.last_refill = now;
        }

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            self.evict_if_needed();
            return Ok(None);
        }

        let remaining = bucket.tokens.floor().max(0.0) as u64;
        let seconds_for_one_token = (1.0 / refill_per_millis / 1000.0).ceil().max(1.0) as u64;
        let reset_at = SystemTime::now()
            .checked_add(Duration::from_secs(seconds_for_one_token))
            .unwrap_or_else(SystemTime::now)
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut response_headers = HeaderMap::new();
        response_headers.insert(X_RATELIMIT_LIMIT, HeaderValue::from(self.capacity));
        response_headers.insert(X_RATELIMIT_REMAINING, HeaderValue::from(remaining));
        response_headers.insert(X_RATELIMIT_RESET, HeaderValue::from(reset_at));
        response_headers.insert(
            header::RETRY_AFTER,
            HeaderValue::from(seconds_for_one_token),
        );
        Ok(Some(response_headers))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct FixedWindowRateLimit {
    pub rate_per_unit: i32,
    pub unit: TimeUnit,
    pub scope: LimitLocation,
    /// `"<scope key>:<window index>" -> request count`.
    #[serde(skip)]
    pub(crate) count_map: HashMap<String, i32>,
}

fn window_index_ms(time_unit: &TimeUnit) -> u64 {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let window_size = time_unit.window_millis();
    (now_ms / window_size) * window_size
}

impl FixedWindowRateLimit {
    fn should_limit(
        &mut self,
        headers: Option<&HeaderMap<HeaderValue>>,
        peer_addr: &SocketAddr,
    ) -> Result<Option<HeaderMap>, AppError> {
        if self.rate_per_unit <= 0 {
            debug!(
                "rate_limit misconfigured (rate_per_unit={}); request is not limited",
                self.rate_per_unit
            );
            return Ok(None);
        }
        let Some(scope_key) = self.scope.bucket_key(headers, peer_addr) else {
            return Ok(None);
        };

        let window_size = self.unit.window_millis();
        let window_start = window_index_ms(&self.unit);
        let key = format!("{scope_key}:{window_start}");

        if self.count_map.len() >= DEFAULT_RATELIMIT_MAX_BUCKETS {
            // The map holds one entry per (scope, window). Drop an arbitrary
            // entry; with a sane cap this only happens under extreme cardinality.
            if let Some(evicted) = self.count_map.keys().next().cloned() {
                self.count_map.remove(&evicted);
            }
        }

        let counter = self.count_map.entry(key.clone()).or_insert(0);
        *counter += 1;
        let remaining = self.rate_per_unit - *counter;

        if remaining >= 0 {
            return Ok(None);
        }

        let reset_at_ms = window_start + window_size;
        let reset_at_secs = reset_at_ms / 1000;
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let retry_after_seconds = reset_at_secs.saturating_sub(now_secs).max(1);

        let mut response_headers = HeaderMap::new();
        response_headers.insert(
            X_RATELIMIT_LIMIT,
            HeaderValue::from(self.rate_per_unit as u64),
        );
        response_headers.insert(X_RATELIMIT_REMAINING, HeaderValue::from(0));
        response_headers.insert(X_RATELIMIT_RESET, HeaderValue::from(reset_at_secs));
        response_headers.insert(header::RETRY_AFTER, HeaderValue::from(retry_after_seconds));

        Ok(Some(response_headers))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn addr(ip: &str) -> SocketAddr {
        SocketAddr::new(ip.parse::<IpAddr>().unwrap(), 1234)
    }

    fn token_bucket(scope: LimitLocation) -> TokenBucketRateLimit {
        TokenBucketRateLimit {
            rate_per_unit: 10,
            unit: TimeUnit::Second,
            capacity: 10,
            scope,
            buckets: HashMap::new(),
        }
    }

    #[test]
    fn test_token_bucket_starts_full() {
        let mut rate_limit = token_bucket(LimitLocation::IP(IPBasedRatelimit::default()));
        let headers = HeaderMap::new();
        let socket_addr = addr("127.0.0.1");

        // A freshly created limiter must allow a full burst, not reject.
        for _ in 0..10 {
            assert!(matches!(
                rate_limit.should_limit(Some(&headers), &socket_addr),
                Ok(None)
            ));
        }
        assert!(matches!(
            rate_limit.should_limit(Some(&headers), &socket_addr),
            Ok(Some(_))
        ));
    }

    #[test]
    fn test_token_bucket_is_per_client_ip() {
        let mut rate_limit = TokenBucketRateLimit {
            rate_per_unit: 1,
            unit: TimeUnit::Second,
            capacity: 1,
            scope: LimitLocation::IP(IPBasedRatelimit::default()),
            buckets: HashMap::new(),
        };
        let headers = HeaderMap::new();
        let client_a = addr("192.168.1.10");
        let client_b = addr("192.168.1.11");

        // Each client gets its own bucket: exhausting A must not affect B.
        assert!(
            rate_limit
                .should_limit(Some(&headers), &client_a)
                .unwrap()
                .is_none()
        );
        assert!(
            rate_limit
                .should_limit(Some(&headers), &client_a)
                .unwrap()
                .is_some()
        );
        assert!(
            rate_limit
                .should_limit(Some(&headers), &client_b)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn test_token_bucket_ignores_unmatched_header_scope() {
        let mut rate_limit = TokenBucketRateLimit {
            rate_per_unit: 1,
            unit: TimeUnit::Second,
            capacity: 1,
            scope: LimitLocation::Header(HeaderBasedRatelimit {
                key: "X-Api-Key".to_string(),
                value: String::new(),
            }),
            buckets: HashMap::new(),
        };
        let socket_addr = addr("127.0.0.1");

        // No header: the rule does not apply at all.
        assert!(
            rate_limit
                .should_limit(Some(&HeaderMap::new()), &socket_addr)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn test_token_bucket_limits_per_header_value() {
        let mut rate_limit = TokenBucketRateLimit {
            rate_per_unit: 1,
            unit: TimeUnit::Second,
            capacity: 1,
            scope: LimitLocation::Header(HeaderBasedRatelimit {
                key: "X-Api-Key".to_string(),
                value: String::new(),
            }),
            buckets: HashMap::new(),
        };
        let socket_addr = addr("127.0.0.1");

        let mut headers = HeaderMap::new();
        headers.insert("X-Api-Key", "key-a".parse().unwrap());
        assert!(
            rate_limit
                .should_limit(Some(&headers), &socket_addr)
                .unwrap()
                .is_none()
        );
        assert!(
            rate_limit
                .should_limit(Some(&headers), &socket_addr)
                .unwrap()
                .is_some()
        );

        let mut other = HeaderMap::new();
        other.insert("X-Api-Key", "key-b".parse().unwrap());
        assert!(
            rate_limit
                .should_limit(Some(&other), &socket_addr)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn test_token_bucket_misconfiguration_does_not_limit() {
        let mut rate_limit = TokenBucketRateLimit::default();
        let headers = HeaderMap::new();
        let socket_addr = addr("127.0.0.1");
        assert!(
            rate_limit
                .should_limit(Some(&headers), &socket_addr)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn test_fixed_window_rate_limit() {
        let mut rate_limit = FixedWindowRateLimit {
            rate_per_unit: 2,
            unit: TimeUnit::Second,
            scope: LimitLocation::IP(IPBasedRatelimit::default()),
            count_map: HashMap::new(),
        };
        let headers = HeaderMap::new();
        let socket_addr = addr("127.0.0.1");

        assert!(
            rate_limit
                .should_limit(Some(&headers), &socket_addr)
                .unwrap()
                .is_none()
        );
        assert!(
            rate_limit
                .should_limit(Some(&headers), &socket_addr)
                .unwrap()
                .is_none()
        );
        assert!(
            rate_limit
                .should_limit(Some(&headers), &socket_addr)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn test_fixed_window_is_per_client_ip() {
        let mut rate_limit = FixedWindowRateLimit {
            rate_per_unit: 1,
            unit: TimeUnit::Second,
            scope: LimitLocation::IP(IPBasedRatelimit::default()),
            count_map: HashMap::new(),
        };
        let headers = HeaderMap::new();
        assert!(
            rate_limit
                .should_limit(Some(&headers), &addr("10.0.0.1"))
                .unwrap()
                .is_none()
        );
        assert!(
            rate_limit
                .should_limit(Some(&headers), &addr("10.0.0.1"))
                .unwrap()
                .is_some()
        );
        assert!(
            rate_limit
                .should_limit(Some(&headers), &addr("10.0.0.2"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn test_ip_range_rate_limit() {
        let range = IpRangeBasedRatelimit {
            value: "192.168.1.0/24".to_string(),
            parsed: OnceLock::new(),
        };
        let mut rate_limit = TokenBucketRateLimit {
            rate_per_unit: 1,
            unit: TimeUnit::Second,
            capacity: 1,
            scope: LimitLocation::Iprange(range),
            buckets: HashMap::new(),
        };
        let headers = HeaderMap::new();

        // Inside the range: limited.
        let inside = addr("192.168.1.55");
        assert!(
            rate_limit
                .should_limit(Some(&headers), &inside)
                .unwrap()
                .is_none()
        );
        assert!(
            rate_limit
                .should_limit(Some(&headers), &inside)
                .unwrap()
                .is_some()
        );

        // Outside the range: the rule does not apply.
        let outside = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 2, 1)), 1234);
        assert!(
            rate_limit
                .should_limit(Some(&headers), &outside)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn test_legacy_ip_scope_value_is_ignored() {
        // Old configs pinned a single address in `value`; that address used to
        // be the *only* limited client. Any client must be limited now.
        let mut rate_limit = TokenBucketRateLimit {
            rate_per_unit: 1,
            unit: TimeUnit::Second,
            capacity: 1,
            scope: LimitLocation::IP(IPBasedRatelimit {
                value: "127.0.0.1".to_string(),
            }),
            buckets: HashMap::new(),
        };
        let headers = HeaderMap::new();
        let client = addr("203.0.113.5");
        assert!(
            rate_limit
                .should_limit(Some(&headers), &client)
                .unwrap()
                .is_none()
        );
        assert!(
            rate_limit
                .should_limit(Some(&headers), &client)
                .unwrap()
                .is_some()
        );
    }
}
