use crate::constants::common_constants::DEFAULT_IP_BAN_MAX_TRACKED_IPS;
use crate::middleware::middlewares::CheckResult;
use crate::middleware::middlewares::Denial;
use crate::middleware::middlewares::Middleware;
use crate::utils::duration_urils::human_duration;
use crate::vojo::app_error::AppError;
use http::HeaderMap;
use http::HeaderValue;
use http::StatusCode;
use http::header;
use ipnet::Ipv4Net;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

const BANNED_BODY: &str = "Your IP address is temporarily banned due to excessive requests";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IpBan {
    pub threshold: u32,
    #[serde(with = "human_duration")]
    pub window: Duration,
    #[serde(with = "human_duration")]
    pub ban_duration: Duration,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub whitelist: Option<Vec<String>>,
    #[serde(skip)]
    counters: HashMap<IpAddr, (Instant, u32)>,
    #[serde(skip)]
    banned: HashMap<IpAddr, Instant>,
    #[serde(skip)]
    parsed_whitelist: Option<Vec<WhitelistEntry>>,
}
#[derive(Debug, Clone, PartialEq)]
enum WhitelistEntry {
    Ip(IpAddr),
    Net(Ipv4Net),
}
impl WhitelistEntry {
    fn matches(&self, ip: IpAddr) -> bool {
        match self {
            WhitelistEntry::Ip(allowed) => *allowed == ip,
            WhitelistEntry::Net(net) => match ip {
                IpAddr::V4(v4) => net.contains(&v4),
                IpAddr::V6(_) => false,
            },
        }
    }
}
impl IpBan {
    fn parse_whitelist(rules: &[String]) -> Result<Vec<WhitelistEntry>, AppError> {
        rules
            .iter()
            .map(|rule| {
                if rule.contains('/') {
                    let net = rule.parse::<Ipv4Net>().map_err(|e| {
                        AppError(format!("Invalid ip_ban whitelist CIDR '{rule}': {e}"))
                    })?;
                    Ok(WhitelistEntry::Net(net))
                } else {
                    let addr = rule.parse::<IpAddr>().map_err(|e| {
                        AppError(format!("Invalid ip_ban whitelist IP '{rule}': {e}"))
                    })?;
                    Ok(WhitelistEntry::Ip(addr))
                }
            })
            .collect()
    }
    fn is_whitelisted(&mut self, ip: IpAddr) -> Result<bool, AppError> {
        let rules = match &self.whitelist {
            Some(rules) if !rules.is_empty() => rules,
            _ => return Ok(false),
        };
        if self.parsed_whitelist.is_none() {
            self.parsed_whitelist = Some(Self::parse_whitelist(rules)?);
        }
        Ok(self
            .parsed_whitelist
            .as_ref()
            .is_some_and(|entries| entries.iter().any(|entry| entry.matches(ip))))
    }
    fn check_banned(&mut self, ip: IpAddr) -> Option<Denial> {
        let banned_until = self.banned.get(&ip).copied()?;
        let now = Instant::now();
        if now >= banned_until {
            self.banned.remove(&ip);
            self.counters.remove(&ip);
            return None;
        }
        debug!("[IpBan] Request from {} denied, still banned", ip);
        Some(deny(
            banned_until.saturating_duration_since(now).as_secs().max(1),
        ))
    }
    fn track(&mut self, ip: IpAddr) -> Option<Denial> {
        let now = Instant::now();
        if !self.counters.contains_key(&ip) {
            self.evict_oldest_counter();
        }
        let count = {
            let counter = self.counters.entry(ip).or_insert((now, 0));
            if now.saturating_duration_since(counter.0) >= self.window {
                *counter = (now, 0);
            }
            counter.1 += 1;
            counter.1
        };
        if count > self.threshold {
            self.counters.remove(&ip);
            self.evict_oldest_ban();
            self.banned.insert(ip, now + self.ban_duration);
            warn!(
                "[IpBan] IP {} banned after {} requests within the window, ban lasts {:?}",
                ip, count, self.ban_duration
            );
            return Some(deny(self.ban_duration.as_secs().max(1)));
        }
        None
    }
    fn evict_oldest_counter(&mut self) {
        if self.counters.len() < DEFAULT_IP_BAN_MAX_TRACKED_IPS {
            return;
        }
        if let Some(oldest) = self
            .counters
            .iter()
            .min_by_key(|(_, (window_start, _))| *window_start)
            .map(|(ip, _)| *ip)
        {
            self.counters.remove(&oldest);
        }
    }
    fn evict_oldest_ban(&mut self) {
        if self.banned.len() < DEFAULT_IP_BAN_MAX_TRACKED_IPS {
            return;
        }
        if let Some(oldest) = self
            .banned
            .iter()
            .min_by_key(|(_, banned_until)| *banned_until)
            .map(|(ip, _)| *ip)
        {
            self.banned.remove(&oldest);
        }
    }
}
fn deny(retry_after_secs: u64) -> Denial {
    let mut headers = HeaderMap::new();
    headers.insert(header::RETRY_AFTER, HeaderValue::from(retry_after_secs));
    Denial {
        status: StatusCode::FORBIDDEN,
        headers,
        body: BANNED_BODY.to_string(),
    }
}
impl Middleware for Arc<Mutex<IpBan>> {
    fn check_request(
        &mut self,
        peer_addr: &SocketAddr,
        _headers: Option<&HeaderMap<HeaderValue>>,
    ) -> Result<CheckResult, AppError> {
        let ip = peer_addr.ip();
        let mut lock = self.lock()?;
        if lock.is_whitelisted(ip)? {
            return Ok(CheckResult::Allowed);
        }
        if let Some(denial) = lock.check_banned(ip) {
            return Ok(CheckResult::Denied(denial));
        }
        match lock.track(ip) {
            Some(denial) => Ok(CheckResult::Denied(denial)),
            None => Ok(CheckResult::Allowed),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn peer(ip: &str) -> SocketAddr {
        format!("{ip}:8080").parse().unwrap()
    }
    fn ban_from_yaml(yaml: &str) -> Arc<Mutex<IpBan>> {
        Arc::new(Mutex::new(serde_yaml::from_str(yaml).unwrap()))
    }
    fn ban_for(threshold: u32, window: &str, ban_duration: &str) -> Arc<Mutex<IpBan>> {
        ban_from_yaml(&format!(
            "threshold: {threshold}\nwindow: {window}\nban_duration: {ban_duration}\n"
        ))
    }
    #[test]
    fn allows_requests_up_to_threshold() {
        let mut ip_ban = ban_for(3, "60s", "60s");
        let socket = peer("192.168.1.10");
        for _ in 0..3 {
            let result = ip_ban.check_request(&socket, None).unwrap();
            assert!(result.is_allowed());
        }
    }
    #[test]
    fn bans_ip_when_threshold_exceeded() {
        let mut ip_ban = ban_for(3, "60s", "60s");
        let socket = peer("192.168.1.10");
        for _ in 0..3 {
            assert!(ip_ban.check_request(&socket, None).unwrap().is_allowed());
        }
        let result = ip_ban.check_request(&socket, None).unwrap();
        assert!(!result.is_allowed());
        let denial = result.get_denial().unwrap();
        assert_eq!(denial.status, StatusCode::FORBIDDEN);
        assert_eq!(denial.body, BANNED_BODY);
        assert!(denial.headers.contains_key(header::RETRY_AFTER));
    }
    #[test]
    fn banned_ip_keeps_being_denied() {
        let mut ip_ban = ban_for(1, "60s", "60s");
        let socket = peer("192.168.1.10");
        assert!(ip_ban.check_request(&socket, None).unwrap().is_allowed());
        for _ in 0..5 {
            assert!(!ip_ban.check_request(&socket, None).unwrap().is_allowed());
        }
    }
    #[test]
    fn auto_unbans_after_ban_duration() {
        let mut ip_ban = ban_for(1, "60s", "100ms");
        let socket = peer("192.168.1.10");
        assert!(ip_ban.check_request(&socket, None).unwrap().is_allowed());
        assert!(!ip_ban.check_request(&socket, None).unwrap().is_allowed());
        thread::sleep(Duration::from_millis(150));
        assert!(ip_ban.check_request(&socket, None).unwrap().is_allowed());
    }
    #[test]
    fn window_expiry_resets_counter() {
        let mut ip_ban = ban_for(2, "200ms", "60s");
        let socket = peer("192.168.1.10");
        assert!(ip_ban.check_request(&socket, None).unwrap().is_allowed());
        assert!(ip_ban.check_request(&socket, None).unwrap().is_allowed());
        thread::sleep(Duration::from_millis(250));
        assert!(ip_ban.check_request(&socket, None).unwrap().is_allowed());
        assert!(ip_ban.check_request(&socket, None).unwrap().is_allowed());
        assert!(!ip_ban.check_request(&socket, None).unwrap().is_allowed());
    }
    #[test]
    fn each_ip_is_tracked_independently() {
        let mut ip_ban = ban_for(1, "60s", "60s");
        let first = peer("192.168.1.10");
        let second = peer("192.168.1.11");
        assert!(ip_ban.check_request(&first, None).unwrap().is_allowed());
        assert!(!ip_ban.check_request(&first, None).unwrap().is_allowed());
        assert!(ip_ban.check_request(&second, None).unwrap().is_allowed());
    }
    #[test]
    fn whitelisted_ip_is_never_banned() {
        let mut ip_ban = ban_from_yaml(
            "threshold: 1\nwindow: 60s\nban_duration: 60s\nwhitelist:\n  - 192.168.0.0/16\n  - 10.0.0.5\n",
        );
        let cidr_socket = peer("192.168.1.10");
        for _ in 0..10 {
            assert!(
                ip_ban
                    .check_request(&cidr_socket, None)
                    .unwrap()
                    .is_allowed()
            );
        }
        let exact_socket = peer("10.0.0.5");
        for _ in 0..10 {
            assert!(
                ip_ban
                    .check_request(&exact_socket, None)
                    .unwrap()
                    .is_allowed()
            );
        }
    }
    #[test]
    fn invalid_whitelist_fails_the_check() {
        let mut ip_ban = ban_from_yaml(
            "threshold: 1\nwindow: 60s\nban_duration: 60s\nwhitelist:\n  - not-an-ip\n",
        );
        let socket = peer("192.168.1.10");
        assert!(ip_ban.check_request(&socket, None).is_err());
    }
    #[test]
    fn deserializes_yaml_config() {
        let config = r#"
threshold: 100
window: 1m
ban_duration: 24h
whitelist:
  - 10.0.0.5
  - 192.168.0.0/16
"#;
        let ip_ban: IpBan = serde_yaml::from_str(config).unwrap();
        assert_eq!(ip_ban.threshold, 100);
        assert_eq!(ip_ban.window, Duration::from_secs(60));
        assert_eq!(ip_ban.ban_duration, Duration::from_secs(86400));
        assert_eq!(
            ip_ban.whitelist,
            Some(vec!["10.0.0.5".to_string(), "192.168.0.0/16".to_string()])
        );
    }
    #[test]
    fn deserializes_yaml_config_without_whitelist() {
        let config = "threshold: 5\nwindow: 30s\nban_duration: 10m\n";
        let ip_ban: IpBan = serde_yaml::from_str(config).unwrap();
        assert_eq!(ip_ban.threshold, 5);
        assert_eq!(ip_ban.window, Duration::from_secs(30));
        assert_eq!(ip_ban.ban_duration, Duration::from_secs(600));
        assert_eq!(ip_ban.whitelist, None);
    }
}
