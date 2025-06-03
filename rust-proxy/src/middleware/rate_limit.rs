use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::constants::common_constants::DEFAULT_FIXEDWINDOW_MAP_SIZE;
use crate::vojo::app_error::AppError;
use core::fmt::Debug;
use http::HeaderMap;
use http::HeaderValue;
use ipnet::Ipv4Net;
use iprange::IpRange;
use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;
use std::net::SocketAddr;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "limiter", rename_all = "PascalCase")]
pub enum Ratelimit {
    TokenBucket(TokenBucketRateLimit),
    FixedWindow(FixedWindowRateLimit),
}
impl Ratelimit {
    pub fn should_limit(
        &mut self,
        headers: &HeaderMap<HeaderValue>,
        peer_addr: &SocketAddr,
    ) -> Result<bool, AppError> {
        match self {
            Ratelimit::TokenBucket(tb) => tb.should_limit(headers, peer_addr),
            Ratelimit::FixedWindow(fw) => fw.should_limit(headers, peer_addr),
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
#[serde(tag = "kind")]
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
    pub rate_per_unit: i32,
    pub unit: TimeUnit,
    pub capacity: i32,
    pub scope: LimitLocation,
    #[serde(skip_serializing, skip_deserializing)]
    pub current_count: i32,
    #[serde(skip_serializing, skip_deserializing, default = "default_time")]
    pub last_update_time: SystemTime,
}
impl Default for TokenBucketRateLimit {
    fn default() -> Self {
        TokenBucketRateLimit {
            last_update_time: SystemTime::now(),
            rate_per_unit: 0,
            unit: TimeUnit::default(),
            capacity: 0,
            scope: LimitLocation::default(),
            current_count: 0,
        }
    }
}
fn default_time() -> SystemTime {
    SystemTime::now()
}
fn get_time_key(time_unit: TimeUnit) -> Result<String, AppError> {
    let current_time = SystemTime::now();
    let since_the_epoch = current_time.duration_since(UNIX_EPOCH)?;
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
    headers: &HeaderMap<HeaderValue>,
    peer_addr: &SocketAddr,
) -> Result<bool, AppError> {
    let remote_ip = peer_addr.ip().to_string();
    match limit_location {
        LimitLocation::IP(ip_based_ratelimit) => Ok(ip_based_ratelimit.value == remote_ip),
        LimitLocation::Header(header_based_ratelimit) => {
            if !headers.contains_key(header_based_ratelimit.key.clone()) {
                return Ok(false);
            }
            let header_value = headers
                .get(header_based_ratelimit.key.clone())
                .ok_or("Can not find the header_based_ratelimit key.")?;
            let header_value_str = header_value.to_str()?;

            Ok(header_value_str == header_based_ratelimit.value)
        }
        LimitLocation::Iprange(ip_range_based_ratelimit) => {
            if !ip_range_based_ratelimit.value.contains('/') {
                return Err(AppError(("The Ip Range should contain '/'.").to_string()));
            }
            let ip_range: IpRange<Ipv4Net> = [ip_range_based_ratelimit.value]
                .iter()
                .map(|s| s.parse::<Ipv4Net>().map_err(|e| AppError(e.to_string())))
                .collect::<Result<IpRange<Ipv4Net>, AppError>>()?;
            let source_ip = remote_ip.parse::<Ipv4Addr>()?;
            Ok(ip_range.contains(&source_ip))
        }
    }
}

impl TokenBucketRateLimit {
    fn should_limit(
        &mut self,
        headers: &HeaderMap<HeaderValue>,
        peer_addr: &SocketAddr,
    ) -> Result<bool, AppError> {
        if !matched(self.scope.clone(), headers, peer_addr)? {
            return Ok(false);
        }

        let now = SystemTime::now();
        let elapsed = now.duration_since(self.last_update_time)?;

        let elapsed_millis = elapsed.as_millis();
        let tokens_to_add =
            (elapsed_millis * self.rate_per_unit as u128) / self.unit.get_million_second();

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
        headers: &HeaderMap<HeaderValue>,
        peer_addr: &SocketAddr,
    ) -> Result<bool, AppError> {
        if !matched(self.limit_location.clone(), headers, peer_addr)? {
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
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn test_token_bucket_rate_limit() {
        let mut headers = HeaderMap::new();
        headers.insert("test-header", "test-value".parse().unwrap());

        let socket_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);

        let mut rate_limit = TokenBucketRateLimit {
            rate_per_unit: 10,
            unit: TimeUnit::Second,
            capacity: 10,
            scope: LimitLocation::IP(IPBasedRatelimit {
                value: "127.0.0.1".to_string(),
            }),
            current_count: 5,
            last_update_time: SystemTime::now(),
        };

        assert!(!rate_limit.should_limit(&headers, &socket_addr).unwrap());

        rate_limit.current_count = 0;
        assert!(rate_limit.should_limit(&headers, &socket_addr).unwrap());
    }

    #[test]
    fn test_fixed_window_rate_limit() {
        let mut headers = HeaderMap::new();
        headers.insert("test-header", "test-value".parse().unwrap());

        let socket_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);

        let mut rate_limit = FixedWindowRateLimit {
            rate_per_unit: 2,
            unit: TimeUnit::Second,
            limit_location: LimitLocation::IP(IPBasedRatelimit {
                value: "127.0.0.1".to_string(),
            }),
            count_map: HashMap::new(),
        };

        assert!(!rate_limit.should_limit(&headers, &socket_addr).unwrap());
        assert!(!rate_limit.should_limit(&headers, &socket_addr).unwrap());

        assert!(rate_limit.should_limit(&headers, &socket_addr).unwrap());
    }

    #[test]
    fn test_ip_range_rate_limit() {
        let headers = HeaderMap::new();
        let socket_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), 8080);

        let mut rate_limit = TokenBucketRateLimit {
            rate_per_unit: 10,
            unit: TimeUnit::Second,
            capacity: 10,
            scope: LimitLocation::Iprange(IpRangeBasedRatelimit {
                value: "192.168.1.0/24".to_string(),
            }),
            current_count: 5,
            last_update_time: SystemTime::now(),
        };

        assert!(!rate_limit.should_limit(&headers, &socket_addr).unwrap());

        let socket_addr_outside = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 2, 1)), 8080);
        assert!(!rate_limit
            .should_limit(&headers, &socket_addr_outside)
            .unwrap());
    }

    #[test]
    fn test_header_based_rate_limit() {
        let mut headers = HeaderMap::new();
        headers.insert("X-API-Key", "test-key".parse().unwrap());

        let socket_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);

        let mut rate_limit = TokenBucketRateLimit {
            rate_per_unit: 10,
            unit: TimeUnit::Second,
            capacity: 10,
            scope: LimitLocation::Header(HeaderBasedRatelimit {
                key: "X-API-Key".to_string(),
                value: "test-key".to_string(),
            }),
            current_count: 5,
            last_update_time: SystemTime::now(),
        };

        assert!(!rate_limit.should_limit(&headers, &socket_addr).unwrap());

        headers.insert("X-API-Key", "wrong-key".parse().unwrap());
        assert!(!rate_limit.should_limit(&headers, &socket_addr).unwrap());
    }
}
