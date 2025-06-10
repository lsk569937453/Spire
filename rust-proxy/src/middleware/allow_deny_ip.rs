use ipnet::Ipv4Net;
use iprange::IpRange;
use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;
use std::net::SocketAddr;

use crate::vojo::app_error::AppError;
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AllowDenyIp {
    pub rules: Vec<AllowDenyItem>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AllowDenyItem {
    pub policy: AllowType,
    pub value: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub enum AllowType {
    #[default]
    AllowAll,
    DenyAll,
    Allow,
    Deny,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub enum AllowResult {
    #[default]
    Allow,
    Deny,
    Notmapping,
}
impl AllowDenyIp {
    pub fn ip_is_allowed(&self, peer_addr: &SocketAddr) -> Result<bool, AppError> {
        let ip = peer_addr.ip().to_string();
        for item in &self.rules {
            let is_allow = item.is_allow(ip.clone());
            match is_allow {
                Ok(AllowResult::Allow) => {
                    return Ok(true);
                }
                Ok(AllowResult::Deny) => {
                    return Ok(false);
                }
                Ok(AllowResult::Notmapping) => {
                    continue;
                }
                Err(err) => {
                    return Err(AppError(err.to_string()));
                }
            }
        }
        Ok(true)
    }
}
impl AllowDenyItem {
    pub fn is_allow(&self, client_ip: String) -> Result<AllowResult, AppError> {
        if self.policy == AllowType::AllowAll {
            return Ok(AllowResult::Allow);
        }
        if self.policy == AllowType::DenyAll {
            return Ok(AllowResult::Deny);
        }
        if self.value.is_none() {
            return Err(AppError::from(
                "the value counld not be none when the limit_type is not AllowAll or DenyAll!",
            ));
        }
        let config_ip = self.value.clone().ok_or("config_ip is none")?;
        let value_mapped_ip: bool = if config_ip.contains('/') {
            let mut ip_range_vec = Vec::new();
            {
                let s = &config_ip;
                let parsed = s.parse()?;
                ip_range_vec.push(parsed);
            }
            let ip_range: IpRange<Ipv4Net> = ip_range_vec.into_iter().collect();
            let source_ip = client_ip.parse::<Ipv4Addr>()?;
            ip_range.contains(&source_ip)
        } else {
            self.value.clone().ok_or("self.value is none")? == client_ip
        };
        if value_mapped_ip && self.policy == AllowType::Allow {
            return Ok(AllowResult::Allow);
        }
        if value_mapped_ip && self.policy == AllowType::Deny {
            return Ok(AllowResult::Deny);
        }

        Ok(AllowResult::Notmapping)
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_allow_allow_all() {
        let allow_object = AllowDenyItem {
            policy: AllowType::AllowAll,
            value: None,
        };
        let result = allow_object.is_allow(String::from("test"));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), AllowResult::Allow);
    }
    #[test]
    fn test_is_allow_deny_all() {
        let allow_object = AllowDenyItem {
            policy: AllowType::DenyAll,
            value: None,
        };
        let result = allow_object.is_allow(String::from("test"));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), AllowResult::Deny);
    }
    #[test]
    fn test_is_allow_allow_ip() {
        let allow_object = AllowDenyItem {
            policy: AllowType::Allow,
            value: Some(String::from("192.168.0.1")),
        };
        let result = allow_object.is_allow(String::from("192.168.0.1"));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), AllowResult::Allow);
    }
    #[test]
    fn test_is_allow_allow_ip_range() {
        let allow_object = AllowDenyItem {
            policy: AllowType::Allow,
            value: Some(String::from("192.168.0.1/24")),
        };
        let result1 = allow_object.is_allow(String::from("192.168.0.254"));
        assert!(result1.is_ok());
        assert_eq!(result1.unwrap(), AllowResult::Allow);

        let result2 = allow_object.is_allow(String::from("192.168.0.1"));
        assert!(result2.is_ok());
        assert_eq!(result2.unwrap(), AllowResult::Allow);
    }
    #[test]
    fn test_is_allow_deny_ip() {
        let allow_object = AllowDenyItem {
            policy: AllowType::Deny,
            value: Some(String::from("192.168.0.1")),
        };
        let result = allow_object.is_allow(String::from("192.168.0.1"));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), AllowResult::Deny);
    }
    #[test]
    fn test_is_allow_deny_ip_range() {
        let allow_object = AllowDenyItem {
            policy: AllowType::Deny,
            value: Some(String::from("192.168.0.1/16")),
        };
        let result1 = allow_object.is_allow(String::from("192.168.255.254"));
        assert!(result1.is_ok());
        assert_eq!(result1.unwrap(), AllowResult::Deny);

        let result2 = allow_object.is_allow(String::from("192.168.0.1"));
        assert!(result2.is_ok());
        assert_eq!(result2.unwrap(), AllowResult::Deny);
    }

    #[test]
    fn test_is_allow_not_mapping1() {
        let allow_object = AllowDenyItem {
            policy: AllowType::Allow,
            value: Some(String::from("192.168.0.1")),
        };
        let result = allow_object.is_allow(String::from("192.168.3.4"));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), AllowResult::Notmapping);
    }
    #[test]
    fn test_is_allow_not_mapping2() {
        let allow_object = AllowDenyItem {
            policy: AllowType::Deny,
            value: Some(String::from("192.168.0.1")),
        };
        let result = allow_object.is_allow(String::from("192.168.3.4"));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), AllowResult::Notmapping);
    }
}
