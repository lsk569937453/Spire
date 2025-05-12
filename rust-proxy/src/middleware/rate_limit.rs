use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::constants::common_constants::DEFAULT_FIXEDWINDOW_MAP_SIZE;
use core::fmt::Debug;
use http::HeaderMap;
use http::HeaderValue;
use ipnet::Ipv4Net;
use iprange::IpRange;
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::net::Ipv4Addr;

use super::app_error::AppError;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "PascalCase")]
pub enum Ratelimit {
    TokenBucket(TokenBucketRateLimit),
    FixedWindow(FixedWindowRateLimit),
}
impl Ratelimit {
    pub fn should_limit(
        &mut self,
        headers: HeaderMap<HeaderValue>,
        remote_ip: String,
    ) -> Result<bool, AppError> {
        match self {
            Ratelimit::TokenBucket(tb) => tb.should_limit(headers, remote_ip),
            Ratelimit::FixedWindow(fw) => fw.should_limit(headers, remote_ip),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IPBasedRatelimit {
    pub value: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeaderBasedRatelimit {
    pub key: String,
    pub value: String,
}
impl HeaderBasedRatelimit {
    fn get_key(&self) -> String {
        format!("{}:{}", self.key, self.value)
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IpRangeBasedRatelimit {
    pub value: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum LimitLocation {
    IP(IPBasedRatelimit),
    Header(HeaderBasedRatelimit),
    Iprange(IpRangeBasedRatelimit),
}
impl Default for LimitLocation {
    fn default() -> Self {
        LimitLocation::IP(IPBasedRatelimit {
            value: String::new(),
        })
    }
}
impl LimitLocation {
    pub fn get_key(&self) -> String {
        match self {
            LimitLocation::Header(headers) => headers.get_key(),
            LimitLocation::IP(ip) => ip.value.clone(),
            LimitLocation::Iprange(ip_range) => ip_range.value.clone(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TimeUnit {
    MillionSecond,
    Second,
    Minute,
    Hour,
    Day,
}

impl TimeUnit {
    pub fn get_million_second(&self) -> u128 {
        match self {
            Self::MillionSecond => 1,
            Self::Second => 1_000,
            Self::Minute => 60_000,
            Self::Hour => 3_600_000,
            Self::Day => 86_400_000,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TokenBucketRateLimit {
    pub rate_per_unit: u128,
    pub unit: TimeUnit,
    pub capacity: i32,
    pub limit_location: LimitLocation,
    #[serde(skip_serializing, skip_deserializing)]
    pub current_count: i32,
    #[serde(skip_serializing, skip_deserializing)]
    pub lock: i32,
    #[serde(skip_serializing, skip_deserializing, default = "default_time")]
    pub last_update_time: SystemTime,
}

fn default_time() -> SystemTime {
    SystemTime::now()
}
fn get_time_key(time_unit: TimeUnit) -> Result<String, AppError> {
    let current_time = SystemTime::now();
    let since_the_epoch = current_time
        .duration_since(UNIX_EPOCH)
        .map_err(|err| AppError(err.to_string()))?;
    let in_ms =
        since_the_epoch.as_secs() * 1000 + since_the_epoch.subsec_nanos() as u64 / 1_000_000;
    let key_u64 = match time_unit {
        TimeUnit::MillionSecond => in_ms,
        TimeUnit::Second => in_ms / 1000,
        TimeUnit::Minute => in_ms / 60000,
        TimeUnit::Hour => in_ms / 3600000,
        TimeUnit::Day => in_ms / 86400000,
    };
    Ok(key_u64.to_string())
}

fn matched(
    limit_location: LimitLocation,
    headers: HeaderMap<HeaderValue>,
    remote_ip: String,
) -> Result<bool, AppError> {
    match limit_location {
        LimitLocation::IP(ip_based_ratelimit) => Ok(ip_based_ratelimit.value == remote_ip),
        LimitLocation::Header(header_based_ratelimit) => {
            if !headers.contains_key(header_based_ratelimit.key.clone()) {
                return Ok(false);
            }
            let header_value = headers.get(header_based_ratelimit.key.clone()).unwrap();
            let header_value_str = header_value
                .to_str()
                .map_err(|err| AppError(err.to_string()))?;

            Ok(header_value_str == header_based_ratelimit.value)
        }
        LimitLocation::Iprange(ip_range_based_ratelimit) => {
            if !ip_range_based_ratelimit.value.contains('/') {
                return Err(AppError(("The Ip Range should contain '/'.").to_string()));
            }
            let ip_range: IpRange<Ipv4Net> = [ip_range_based_ratelimit.value]
                .iter()
                .map(|s| s.parse().unwrap())
                .collect();
            let source_ip = remote_ip
                .parse::<Ipv4Addr>()
                .map_err(|err| AppError(err.to_string()))?;
            Ok(ip_range.contains(&source_ip))
        }
    }
}

impl TokenBucketRateLimit {
    fn should_limit(
        &mut self,
        headers: HeaderMap<HeaderValue>,
        remote_ip: String,
    ) -> Result<bool, AppError> {
        if !matched(self.limit_location.clone(), headers, remote_ip)? {
            return Ok(false);
        }

        let now = SystemTime::now();
        let elapsed = now
            .duration_since(self.last_update_time)
            .map_err(|err| AppError(err.to_string()))?;

        let elapsed_millis = elapsed.as_millis();
        let tokens_to_add = (elapsed_millis * self.rate_per_unit) / self.unit.get_million_second();

        if tokens_to_add > 0 {
            self.current_count = (self.current_count + tokens_to_add as i32).min(self.capacity);
            self.last_update_time = now;
        }

        if self.current_count > 0 {
            self.current_count -= 1;
            Ok(false) // Not limited
        } else {
            Ok(true) // Limited
        }
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]

pub struct FixedWindowRateLimit {
    pub rate_per_unit: u128,
    pub unit: TimeUnit,
    pub limit_location: LimitLocation,
    #[serde(skip_serializing, skip_deserializing)]
    pub count_map: HashMap<String, i32>,
}
impl FixedWindowRateLimit {
    fn should_limit(
        &mut self,
        headers: HeaderMap<HeaderValue>,
        remote_ip: String,
    ) -> Result<bool, AppError> {
        // Check if request matches our limiting criteria
        if !matched(self.limit_location.clone(), headers, remote_ip)? {
            return Ok(false);
        }

        let time_unit_key = get_time_key(self.unit.clone())?;
        let location_key = self.limit_location.get_key();
        let key = format!("{}:{}", location_key, time_unit_key);

        if self.count_map.len() >= DEFAULT_FIXEDWINDOW_MAP_SIZE as usize {
            if let Some(oldest_key) = self.count_map.keys().next().cloned() {
                self.count_map.remove(&oldest_key);
            }
        }
        let counter = self.count_map.entry(key).or_insert(0);
        *counter += 1;
        Ok(*counter > self.rate_per_unit as i32)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;
    
    use http::{HeaderName, HeaderValue};
    fn create_headers(key: &str, value: &str) -> HeaderMap<HeaderValue> {
        let mut headers = HeaderMap::new();

        let ss = HeaderName::from_str(key).unwrap();
        headers.insert(ss, HeaderValue::from_str(value).unwrap());
        headers
    }

    #[test]
    fn test_token_bucket_basic() {
        let mut tb = TokenBucketRateLimit {
            rate_per_unit: 1,
            unit: TimeUnit::Second,
            capacity: 2,
            limit_location: LimitLocation::IP(IPBasedRatelimit {
                value: "127.0.0.1".to_string(),
            }),
            current_count: 2,
            lock: 0,
            last_update_time: UNIX_EPOCH,
        };

        // First request should pass
        assert!(!tb
            .should_limit(HeaderMap::new(), "127.0.0.1".to_string())
            .unwrap());
        // Second request should pass
        assert!(!tb
            .should_limit(HeaderMap::new(), "127.0.0.1".to_string())
            .unwrap());
        // Third request should be limited
        assert!(tb
            .should_limit(HeaderMap::new(), "127.0.0.1".to_string())
            .unwrap());
    }

    #[test]
    fn test_fixed_window_basic() {
        let mut fw = FixedWindowRateLimit {
            rate_per_unit: 2,
            unit: TimeUnit::Second,
            limit_location: LimitLocation::Header(HeaderBasedRatelimit {
                key: "X-User".to_string(),
                value: "test".to_string(),
            }),
            count_map: HashMap::new(),
        };

        let headers = create_headers("X-User", "test");

        // First request
        assert!(!fw
            .should_limit(headers.clone(), "127.0.0.1".to_string())
            .unwrap());
        // Second request
        assert!(!fw
            .should_limit(headers.clone(), "127.0.0.1".to_string())
            .unwrap());
        // Third request should be limited
        assert!(fw.should_limit(headers, "127.0.0.1".to_string()).unwrap());
    }

    #[test]
    fn test_ip_matching() {
        let ip_limit = LimitLocation::IP(IPBasedRatelimit {
            value: "192.168.1.1".to_string(),
        });

        // Matching IP
        assert!(matched(
            ip_limit.clone(),
            HeaderMap::new(),
            "192.168.1.1".to_string()
        )
        .unwrap());
        // Non-matching IP
        assert!(!matched(ip_limit, HeaderMap::new(), "10.0.0.1".to_string()).unwrap());
    }

    #[test]
    fn test_header_matching() {
        let header_limit = LimitLocation::Header(HeaderBasedRatelimit {
            key: "Authorization".to_string(),
            value: "Bearer token".to_string(),
        });

        // Matching header
        let headers = create_headers("Authorization", "Bearer token");
        assert!(matched(header_limit.clone(), headers, "127.0.0.1".to_string()).unwrap());

        // Non-matching value
        let headers = create_headers("Authorization", "Invalid");
        assert!(!matched(header_limit.clone(), headers, "127.0.0.1".to_string()).unwrap());

        // Missing header
        assert!(!matched(header_limit, HeaderMap::new(), "127.0.0.1".to_string()).unwrap());
    }

    #[test]
    fn test_ip_range_matching() {
        let ip_range_limit = LimitLocation::Iprange(IpRangeBasedRatelimit {
            value: "192.168.1.0/24".to_string(),
        });

        // IP in range
        assert!(matched(
            ip_range_limit.clone(),
            HeaderMap::new(),
            "192.168.1.100".to_string()
        )
        .unwrap());

        // IP out of range
        assert!(!matched(ip_range_limit, HeaderMap::new(), "192.168.2.1".to_string()).unwrap());
    }

    #[test]
    fn test_fixed_window_map_eviction() {
        let mut fw = FixedWindowRateLimit {
            rate_per_unit: 1,
            unit: TimeUnit::MillionSecond,
            limit_location: LimitLocation::IP(IPBasedRatelimit {
                value: "127.0.0.1".to_string(),
            }),
            count_map: HashMap::with_capacity(DEFAULT_FIXEDWINDOW_MAP_SIZE as usize),
        };

        // Fill the map to capacity
        for i in 0..DEFAULT_FIXEDWINDOW_MAP_SIZE {
            let key = format!("key{}", i);
            fw.count_map.insert(key, 1);
        }

        // Add one more entry should evict the oldest
        fw.should_limit(HeaderMap::new(), "127.0.0.1".to_string())
            .unwrap();
        assert_eq!(fw.count_map.len(), DEFAULT_FIXEDWINDOW_MAP_SIZE as usize);
    }

    #[test]
    fn test_token_refill() {
        let mut tb = TokenBucketRateLimit {
            rate_per_unit: 2, // 2 tokens per second
            unit: TimeUnit::Second,
            capacity: 4,
            limit_location: LimitLocation::default(),
            current_count: 0,
            lock: 0,
            last_update_time: SystemTime::now(),
        };

        // Simulate 500ms passing
        tb.last_update_time = SystemTime::now() - std::time::Duration::from_millis(500);
        tb.should_limit(HeaderMap::new(), "127.0.0.1".to_string())
            .unwrap();

        // Should have refilled 1 token (500ms * 2 tokens/s = 1 token)
        assert_eq!(tb.current_count, 0); // Because we used the refilled token
    }

    #[test]
    fn test_invalid_ip_range() {
        let ip_range_limit = LimitLocation::Iprange(IpRangeBasedRatelimit {
            value: "invalid_ip".to_string(),
        });

        let result = matched(ip_range_limit, HeaderMap::new(), "192.168.1.1".to_string());
        assert!(result.is_err());
    }
}
