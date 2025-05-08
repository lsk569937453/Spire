use super::allow_deny_ip::AllowResult;

use crate::vojo::allow_deny_ip::AllowDenyObject;
use crate::vojo::anomaly_detection::AnomalyDetectionType;
use crate::vojo::app_error::AppError;
use crate::vojo::authentication::Authentication;
use crate::vojo::health_check::HealthCheckType;
use crate::vojo::rate_limit::Ratelimit;
use crate::vojo::route::LoadbalancerStrategy;
use http::HeaderMap;
use http::HeaderValue;
use regex::Regex;
use serde::Deserializer;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::sync::mpsc;
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Matcher {
    pub prefix: String,
    pub prefix_rewrite: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct LivenessConfig {
    pub min_liveness_count: i32,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct LivenessStatus {
    pub current_liveness_count: i32,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Route {
    #[serde(skip_deserializing, skip_serializing)]
    pub route_id: String,
    pub host_name: Option<String>,
    pub matcher: Option<Matcher>,
    pub allow_deny_list: Option<Vec<AllowDenyObject>>,
    pub authentication: Option<Authentication>,
    pub anomaly_detection: Option<AnomalyDetectionType>,
    #[serde(skip_deserializing, skip_serializing)]
    pub liveness_status: LivenessStatus,
    pub rewrite_headers: Option<HashMap<String, String>>,
    pub liveness_config: Option<LivenessConfig>,
    pub health_check: Option<HealthCheckType>,
    pub ratelimit: Option<Ratelimit>,
    pub route_cluster: LoadbalancerStrategy,
}

impl Route {
    pub fn is_matched(
        &self,
        path: String,
        headers_option: Option<HeaderMap<HeaderValue>>,
    ) -> Result<Option<String>, AppError> {
        let matcher = self
            .clone()
            .matcher
            .ok_or("The matcher counld not be none for http")
            .map_err(|err| AppError(err.to_string()))?;

        let match_res = path.strip_prefix(matcher.prefix.as_str());
        if match_res.is_none() {
            return Ok(None);
        }
        let final_path = format!("{}{}", matcher.prefix_rewrite, match_res.unwrap());
        // info!("final_path:{}", final_path);
        if let Some(real_host_name) = &self.host_name {
            if headers_option.is_none() {
                return Ok(None);
            }
            let header_map = headers_option.unwrap();
            let host_option = header_map.get("Host");
            if host_option.is_none() {
                return Ok(None);
            }
            let host_result = host_option.unwrap().to_str();
            if host_result.is_err() {
                return Ok(None);
            }
            let host_name_regex =
                Regex::new(real_host_name.as_str()).map_err(|e| AppError(e.to_string()))?;
            return host_name_regex
                .captures(host_result.unwrap())
                .map_or(Ok(None), |_| Ok(Some(final_path)));
        }
        Ok(Some(final_path))
    }
    pub async fn is_allowed(
        &self,
        ip: String,
        headers_option: Option<HeaderMap<HeaderValue>>,
    ) -> Result<bool, AppError> {
        let mut is_allowed = ip_is_allowed(self.allow_deny_list.clone(), ip.clone())?;
        if !is_allowed {
            return Ok(is_allowed);
        }
        if let (Some(header_map), Some(mut authentication_strategy)) =
            (headers_option.clone(), self.authentication.clone())
        {
            is_allowed = authentication_strategy.check_authentication(header_map)?;
            if !is_allowed {
                return Ok(is_allowed);
            }
        }
        if let (Some(header_map), Some(mut ratelimit_strategy)) =
            (headers_option, self.ratelimit.clone())
        {
            is_allowed = !ratelimit_strategy.should_limit(header_map, ip).await?;
        }
        Ok(is_allowed)
    }
}
pub fn ip_is_allowed(
    allow_deny_list: Option<Vec<AllowDenyObject>>,
    ip: String,
) -> Result<bool, AppError> {
    if allow_deny_list.is_none() || allow_deny_list.clone().unwrap().is_empty() {
        return Ok(true);
    }
    let allow_deny_list = allow_deny_list.unwrap();
    // let iter = allow_deny_list.iter();

    for item in allow_deny_list {
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default, strum_macros::Display)]
pub enum ServiceType {
    #[default]
    Http,
    Https,
    Tcp,
    Http2,
    Http2Tls,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ServiceConfig {
    pub server_type: ServiceType,
    pub cert_str: Option<String>,
    pub key_str: Option<String>,
    pub routes: Vec<Route>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiService {
    pub listen_port: i32,
    #[serde(skip_deserializing, skip_serializing)]
    pub api_service_id: String,
    pub service_config: ServiceConfig,
    #[serde(skip_deserializing, skip_serializing)]
    pub sender: mpsc::Sender<()>,
}
impl<'de> Deserialize<'de> for ApiService {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct ApiServiceWithoutSender {
            listen_port: i32,
            service_config: ServiceConfig,
        }

        let api_service_without_sender = ApiServiceWithoutSender::deserialize(deserializer)?;
        let (sender, _) = mpsc::channel(1); // Create a new channel for the deserialized instance

        Ok(ApiService {
            listen_port: api_service_without_sender.listen_port,
            api_service_id: Default::default(),
            service_config: api_service_without_sender.service_config,
            sender,
        })
    }
}
impl PartialEq for ApiService {
    fn eq(&self, other: &Self) -> bool {
        self.listen_port == other.listen_port
            && self.api_service_id == other.api_service_id
            && self.service_config == other.service_config
        // sender 被显式跳过
    }
}
impl Default for ApiService {
    fn default() -> Self {
        let (sender, _) = mpsc::channel(1); // Buffer size 1

        Self {
            listen_port: Default::default(),
            api_service_id: Default::default(),
            service_config: Default::default(),
            sender,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StaticConifg {
    pub access_log: Option<String>,
    pub database_url: Option<String>,
    pub admin_port: i32,
    pub config_file_path: Option<String>,
}
impl Default for StaticConifg {
    fn default() -> Self {
        Self {
            access_log: Some("vojo.log".to_string()),
            database_url: Some("sqlite://vojo.db".to_string()),
            admin_port: 8080,
            config_file_path: Some("vojo_config.yaml".to_string()),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AppConfig {
    #[serde(default)]
    pub static_config: StaticConifg,
    #[serde(rename = "services", deserialize_with = "deserialize_service_config")]
    pub api_service_config: HashMap<i32, ApiService>,
}
fn deserialize_service_config<'de, D>(deserializer: D) -> Result<HashMap<i32, ApiService>, D::Error>
where
    D: Deserializer<'de>,
{
    let services: Vec<ApiService> = Deserialize::deserialize(deserializer)?;

    let mut hashmap = HashMap::new();
    for item in services {
        hashmap.insert(item.listen_port, item);
    }
    Ok(hashmap)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_config_serialize() {
        // 创建测试用的AppConfig实例
        let mut app_config = AppConfig::default();
        let mut api_service = ApiService::default();
        api_service.listen_port = 8080;
        api_service.api_service_id = "test_service".to_string();
        app_config.api_service_config.insert(8080, api_service);

        // 序列化为YAML
        let yaml_str = serde_yaml::to_string(&app_config).unwrap();
        println!("{}", yaml_str);

        // 反序列化YAML
        let deserialized_config: AppConfig = serde_yaml::from_str(&yaml_str).unwrap();

        // 验证序列化和反序列化结果是否一致
        assert_eq!(app_config, deserialized_config);
        assert_eq!(
            deserialized_config
                .api_service_config
                .get(&8080)
                .unwrap()
                .listen_port,
            8080
        );
        assert_eq!(
            deserialized_config
                .api_service_config
                .get(&8080)
                .unwrap()
                .api_service_id,
            "test_service"
        );
    }

    #[test]
    fn test_app_config_serialize_with_static_config() {
        // 创建包含静态配置的AppConfig实例
        let mut app_config = AppConfig::default();
        app_config.static_config.admin_port = 9090;
        app_config.static_config.access_log = Some("/var/log/proxy.log".to_string());

        // 序列化为JSON
        let json_str = serde_json::to_string(&app_config).unwrap();

        // 反序列化JSON
        let deserialized_config: AppConfig = serde_json::from_str(&json_str).unwrap();

        // 验证静态配置是否正确序列化和反序列化
        assert_eq!(app_config, deserialized_config);
        assert_eq!(deserialized_config.static_config.admin_port, 9090);
        assert_eq!(
            deserialized_config.static_config.access_log,
            Some("/var/log/proxy.log".to_string())
        );
    }
}
