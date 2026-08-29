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
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

const BANNED_BODY: &str = "Your IP address is temporarily banned due to excessive requests";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BanRule {
    pub threshold: u32,
    #[serde(with = "human_duration")]
    pub window: Duration,
    #[serde(with = "human_duration")]
    pub ban_duration: Duration,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IpBan {
    pub rules: Vec<BanRule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub whitelist: Option<Vec<String>>,
    #[serde(skip)]
    counters: HashMap<IpAddr, Vec<(Instant, u32)>>,
    #[serde(skip)]
    banned: HashMap<IpAddr, Instant>,
    #[serde(skip)]
    parsed_whitelist: Option<Vec<WhitelistEntry>>,
}

// Accepts both the `rules` list and the legacy flat single-rule format
// (threshold/window/ban_duration at the top level).
impl<'de> Deserialize<'de> for IpBan {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Raw {
            threshold: Option<u32>,
            window: Option<String>,
            ban_duration: Option<String>,
            rules: Option<Vec<BanRule>>,
            whitelist: Option<Vec<String>>,
        }
        let raw = Raw::deserialize(deserializer)?;
        let legacy_present =
            raw.threshold.is_some() || raw.window.is_some() || raw.ban_duration.is_some();
        let rules = match raw.rules {
            Some(rules) => {
                if rules.is_empty() {
                    return Err(serde::de::Error::custom("ip_ban 'rules' must not be empty"));
                }
                if legacy_present {
                    return Err(serde::de::Error::custom(
                        "ip_ban cannot mix 'rules' with top-level threshold/window/ban_duration",
                    ));
                }
                rules
            }
            None => {
                if !legacy_present {
                    return Err(serde::de::Error::custom(
                        "ip_ban requires either 'rules' or top-level threshold/window/ban_duration",
                    ));
                }
                let threshold = raw
                    .threshold
                    .ok_or_else(|| serde::de::Error::missing_field("threshold"))?;
                let window = raw
                    .window
                    .as_deref()
                    .map(human_duration::parse_duration_str)
                    .transpose()
                    .map_err(serde::de::Error::custom)?
                    .ok_or_else(|| serde::de::Error::missing_field("window"))?;
                let ban_duration = raw
                    .ban_duration
                    .as_deref()
                    .map(human_duration::parse_duration_str)
                    .transpose()
                    .map_err(serde::de::Error::custom)?
                    .ok_or_else(|| serde::de::Error::missing_field("ban_duration"))?;
                vec![BanRule {
                    threshold,
                    window,
                    ban_duration,
                }]
            }
        };
        Ok(IpBan {
            rules,
            whitelist: raw.whitelist,
            counters: HashMap::new(),
            banned: HashMap::new(),
            parsed_whitelist: None,
        })
    }
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
            // Counters are kept: rules whose window is still running keep their
            // tally across a ban triggered by another rule.
            self.banned.remove(&ip);
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
        let rule_count = self.rules.len();
        let counters = self
            .counters
            .entry(ip)
            .or_insert_with(|| vec![(now, 0); rule_count]);
        let mut triggered: Vec<usize> = Vec::new();
        for (idx, rule) in self.rules.iter().enumerate() {
            let counter = &mut counters[idx];
            if now.saturating_duration_since(counter.0) >= rule.window {
                *counter = (now, 0);
            }
            counter.1 += 1;
            if counter.1 > rule.threshold {
                triggered.push(idx);
            }
        }
        if triggered.is_empty() {
            return None;
        }
        let worst = *triggered
            .iter()
            .max_by_key(|&&i| self.rules[i].ban_duration)
            .unwrap();
        let (window, ban_duration, count) = {
            let rule = &self.rules[worst];
            (rule.window, rule.ban_duration, counters[worst].1)
        };
        // Reset only the triggered rules so each ban starts a fresh window,
        // while other rules keep counting towards their own threshold.
        for &i in &triggered {
            counters[i] = (now, 0);
        }
        self.evict_oldest_ban();
        self.banned.insert(ip, now + ban_duration);
        warn!(
            "[IpBan] IP {} banned after {} requests within the window {:?}, ban lasts {:?}",
            ip, count, window, ban_duration
        );
        Some(deny(ban_duration.as_secs().max(1)))
    }
    fn evict_oldest_counter(&mut self) {
        if self.counters.len() < DEFAULT_IP_BAN_MAX_TRACKED_IPS {
            return;
        }
        if let Some(oldest) = self
            .counters
            .iter()
            .min_by_key(|(_, counters)| {
                counters
                    .iter()
                    .map(|(window_start, _)| *window_start)
                    .min()
            })
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
    fn short_window_rule_bans_before_long_window_rule() {
        let mut ip_ban = ban_from_yaml(
            "rules:\n  - threshold: 2\n    window: 200ms\n    ban_duration: 60s\n  - threshold: 5\n    window: 60s\n    ban_duration: 60s\n",
        );
        let socket = peer("192.168.1.10");
        assert!(ip_ban.check_request(&socket, None).unwrap().is_allowed());
        assert!(ip_ban.check_request(&socket, None).unwrap().is_allowed());
        // The short-window rule trips on the 3rd request while the long-window
        // rule has only seen 3 of its 5 allowed requests.
        assert!(!ip_ban.check_request(&socket, None).unwrap().is_allowed());
        assert!(!ip_ban.check_request(&socket, None).unwrap().is_allowed());
    }
    #[test]
    fn short_window_reset_keeps_long_window_counting() {
        let mut ip_ban = ban_from_yaml(
            "rules:\n  - threshold: 2\n    window: 100ms\n    ban_duration: 100ms\n  - threshold: 3\n    window: 60s\n    ban_duration: 60s\n",
        );
        let socket = peer("192.168.1.10");
        assert!(ip_ban.check_request(&socket, None).unwrap().is_allowed());
        assert!(ip_ban.check_request(&socket, None).unwrap().is_allowed());
        // Banned by the short-window rule; the long-window tally keeps req 1-3.
        assert!(!ip_ban.check_request(&socket, None).unwrap().is_allowed());
        thread::sleep(Duration::from_millis(150));
        // Short ban expired and its window reset, but the long-window rule has
        // now seen 4 requests, exceeding its threshold of 3.
        assert!(!ip_ban.check_request(&socket, None).unwrap().is_allowed());
        assert!(!ip_ban.check_request(&socket, None).unwrap().is_allowed());
    }
    #[test]
    fn deserializes_multi_rule_yaml_config() {
        let config = r#"
rules:
  - threshold: 1000
    window: 1m
    ban_duration: 10m
  - threshold: 100000
    window: 24h
    ban_duration: 24h
whitelist:
  - 10.0.0.5
  - 192.168.0.0/16
"#;
        let ip_ban: IpBan = serde_yaml::from_str(config).unwrap();
        assert_eq!(
            ip_ban.rules,
            vec![
                BanRule {
                    threshold: 1000,
                    window: Duration::from_secs(60),
                    ban_duration: Duration::from_secs(600),
                },
                BanRule {
                    threshold: 100000,
                    window: Duration::from_secs(86400),
                    ban_duration: Duration::from_secs(86400),
                },
            ]
        );
        assert_eq!(
            ip_ban.whitelist,
            Some(vec!["10.0.0.5".to_string(), "192.168.0.0/16".to_string()])
        );
    }
    #[test]
    fn deserializes_legacy_flat_yaml_config() {
        let config = r#"
threshold: 100
window: 1m
ban_duration: 24h
whitelist:
  - 10.0.0.5
  - 192.168.0.0/16
"#;
        let ip_ban: IpBan = serde_yaml::from_str(config).unwrap();
        assert_eq!(
            ip_ban.rules,
            vec![BanRule {
                threshold: 100,
                window: Duration::from_secs(60),
                ban_duration: Duration::from_secs(86400),
            }]
        );
        assert_eq!(
            ip_ban.whitelist,
            Some(vec!["10.0.0.5".to_string(), "192.168.0.0/16".to_string()])
        );
    }
    #[test]
    fn deserializes_yaml_config_without_whitelist() {
        let config = "threshold: 5\nwindow: 30s\nban_duration: 10m\n";
        let ip_ban: IpBan = serde_yaml::from_str(config).unwrap();
        assert_eq!(
            ip_ban.rules,
            vec![BanRule {
                threshold: 5,
                window: Duration::from_secs(30),
                ban_duration: Duration::from_secs(600),
            }]
        );
        assert_eq!(ip_ban.whitelist, None);
    }
    #[test]
    fn rejects_empty_rules() {
        let config = "rules: []\n";
        assert!(serde_yaml::from_str::<IpBan>(config).is_err());
    }
    #[test]
    fn rejects_mixing_rules_with_legacy_fields() {
        let config = "rules:\n  - threshold: 2\n    window: 1m\n    ban_duration: 10m\nthreshold: 5\n";
        assert!(serde_yaml::from_str::<IpBan>(config).is_err());
    }
    #[test]
    fn rejects_missing_rules_and_legacy_fields() {
        assert!(serde_yaml::from_str::<IpBan>("whitelist:\n  - 10.0.0.5\n").is_err());
        assert!(serde_yaml::from_str::<IpBan>("threshold: 5\nwindow: 1m\n").is_err());
    }
}
