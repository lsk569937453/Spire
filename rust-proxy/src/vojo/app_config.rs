use super::allow_deny_ip::AllowResult;

use crate::constants::common_constants::DEFAULT_ADMIN_PORT;
use crate::constants::common_constants::DEFAULT_LOG_LEVEL;
use crate::vojo::allow_deny_ip::AllowDenyObject;
use crate::vojo::anomaly_detection::AnomalyDetectionType;
use crate::vojo::app_error::AppError;
use crate::vojo::health_check::HealthCheckType;
use crate::vojo::rate_limit::Ratelimit;
use crate::vojo::route::HeaderBasedRoute;
use crate::vojo::route::LoadbalancerStrategy;
use http::HeaderMap;
use http::HeaderValue;
use regex::Regex;
use serde::Deserializer;
use serde::Serializer;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use tokio::sync::mpsc;
use tracing_subscriber::filter::LevelFilter;
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AppConfig {
    #[serde(
        deserialize_with = "deserialize_static_config",
        skip_serializing,
        default
    )]
    pub static_config: StaticConifg,
    #[serde(rename = "services", deserialize_with = "deserialize_service_config")]
    pub api_service_config: HashMap<i32, ApiService>,
}
fn deserialize_static_config<'de, D>(deserializer: D) -> Result<StaticConifg, D::Error>
where
    D: Deserializer<'de>,
{
    info!("deserialize_static_config");
    let mut static_config = StaticConifg::deserialize(deserializer)?;
    if static_config.access_log.is_none() {
        static_config.access_log = Some("".to_string());
    }
    if static_config.database_url.is_none() {
        static_config.database_url = Some("".to_string());
    }
    if static_config.admin_port.is_none() {
        static_config.admin_port = Some(DEFAULT_ADMIN_PORT);
    }
    if static_config.config_file_path.is_none() {
        static_config.config_file_path = Some("".to_string());
    }
    if static_config.log_level.is_none() {
        static_config.log_level = Some(DEFAULT_LOG_LEVEL);
    }
    Ok(static_config)
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
fn is_empty(value: &str) -> bool {
    value.is_empty()
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RouteConfig {
    #[serde(skip_serializing_if = "is_empty", default = "default_route_id")]
    pub route_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matcher: Option<Matcher>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub anomaly_detection: Option<AnomalyDetectionType>,
    #[serde(skip_deserializing, skip_serializing)]
    pub liveness_status: LivenessStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rewrite_headers: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub liveness_config: Option<LivenessConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health_check: Option<HealthCheckType>,
    #[serde(deserialize_with = "deserialize_router", rename = "forward_to")]
    pub router: Router,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub middlewares: Option<Vec<MiddleWares>>,
}

fn default_route_id() -> String {
    get_uuid()
}

impl RouteConfig {
    pub fn is_matched(
        &mut self,
        path: &str,
        headers_option: Option<&HeaderMap<HeaderValue>>,
    ) -> Result<Option<String>, AppError> {
        let matcher = self
            .matcher
            .as_mut()
            .ok_or("The matcher counld not be none for http")?;

        let match_res = path.strip_prefix(matcher.prefix.as_str());
        if match_res.is_none() {
            return Ok(None);
        }
        let final_path = [
            matcher.prefix_rewrite.as_str(),
            match_res.ok_or("match_res is none")?,
        ]
        .join("");
        // info!("final_path:{}", final_path);
        if let Some(real_host_name) = &self.host_name {
            if headers_option.is_none() {
                return Ok(None);
            }
            let header_map = headers_option.ok_or("headers_option is none")?;
            let host_option = header_map.get("Host");
            if host_option.is_none() {
                return Ok(None);
            }
            let host_result = host_option.ok_or("host_option is none")?.to_str();
            if host_result.is_err() {
                return Ok(None);
            }
            let host_name_regex = Regex::new(real_host_name.as_str())?;
            return host_name_regex
                .captures(host_result?)
                .map_or(Ok(None), |_| Ok(Some(final_path)));
        }
        Ok(Some(final_path))
    }
    pub fn is_allowed(
        &self,
        ip: String,
        headers_option: Option<HeaderMap<HeaderValue>>,
    ) -> Result<bool, AppError> {
        if let Some(middlewares) = &mut self.middlewares {
            for middleware in middlewares.iter_mut() {
                let is_allowed = middleware.is_allowed(peer_addr, headers_option)?;
                if !is_allowed {
                    return Ok(is_allowed);
                }
            }
        }
        if let (Some(header_map), Some(mut ratelimit_strategy)) =
            (headers_option, self.ratelimit.clone())
        {
            is_allowed = !ratelimit_strategy.should_limit(header_map, ip)?;
        }
        Ok(is_allowed)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default, strum_macros::Display)]
pub enum ServiceType {
    #[default]
    #[serde(rename = "http")]
    Http,
    #[serde(rename = "https")]
    Https,
    #[serde(rename = "tcp")]
    Tcp,
    #[serde(rename = "tls")]
    Http2,
    Http2Tls,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiService {
    #[serde(rename = "listen")]
    pub listen_port: i32,

    #[serde(rename = "protocol")]
    pub server_type: ServiceType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cert_str: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_str: Option<String>,
    #[serde(rename = "routes")]
    pub route_configs: Vec<RouteConfig>,
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
            #[serde(rename = "listen")]
            port: i32,
            #[serde(rename = "protocol")]
            pub server_type: ServiceType,
            #[serde(skip_serializing_if = "Option::is_none")]
            pub cert_str: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            pub key_str: Option<String>,
            #[serde(rename = "routes")]
            pub route_configs: Vec<RouteConfig>,
        }

        let api_service_without_sender = ApiServiceWithoutSender::deserialize(deserializer)?;
        let (sender, _) = mpsc::channel(1); // Create a new channel for the deserialized instance

        Ok(ApiService {
            listen_port: api_service_without_sender.port,
            server_type: api_service_without_sender.server_type,
            cert_str: api_service_without_sender.cert_str,
            key_str: api_service_without_sender.key_str,
            route_configs: api_service_without_sender.route_configs,
            sender,
        })
    }
}
impl PartialEq for ApiService {
    fn eq(&self, other: &Self) -> bool {
        self.listen_port == other.listen_port
            && self.server_type == other.server_type
            && self.cert_str == other.cert_str
            && self.key_str == other.key_str
            && self.route_configs == other.route_configs
    }
}
impl Default for ApiService {
    fn default() -> Self {
        let (sender, _) = mpsc::channel(1); // Buffer size 1

        Self {
            listen_port: Default::default(),
            server_type: Default::default(),
            cert_str: Default::default(),
            key_str: Default::default(),
            route_configs: Default::default(),

            sender,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StaticConifg {
    pub access_log: Option<String>,
    pub database_url: Option<String>,
    pub admin_port: Option<i32>,
    pub config_file_path: Option<String>,
    pub log_level: Option<LogLevel>,
}
impl Default for StaticConifg {
    fn default() -> Self {
        Self {
            access_log: Some("".to_string()),
            database_url: Some("".to_string()),
            admin_port: Some(DEFAULT_ADMIN_PORT),
            config_file_path: Some("".to_string()),
            log_level: Some(DEFAULT_LOG_LEVEL),
        }
    }
}
impl StaticConifg {
    pub fn get_log_level(&self) -> LevelFilter {
        match self.log_level {
            Some(LogLevel::Debug) => LevelFilter::DEBUG,
            Some(LogLevel::Info) => LevelFilter::INFO,
            Some(LogLevel::Error) => LevelFilter::ERROR,
            Some(LogLevel::Warn) => LevelFilter::WARN,
            None => LevelFilter::INFO,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LogLevel {
    #[serde(rename = "debug")]
    Debug,
    #[serde(rename = "info")]
    Info,
    #[serde(rename = "error")]
    Error,
    #[serde(rename = "warn")]
    Warn,
}

#[cfg(test)]
mod tests {
    use hyper::service;

    use super::*;
    use crate::vojo::route::BaseRoute;
    use crate::vojo::route::HeaderValueMappingType;
    use crate::vojo::route::LoadbalancerStrategy::WeightBased;
    use crate::vojo::route::TextMatch;
    use crate::vojo::route::WeightBasedRoute;
    use crate::vojo::route::WeightRoute;
    use crate::vojo::route::{self, HeaderRoute};

    #[test]
    fn test_route_matching() {
        let mut route = RouteConfig {
            matcher: Some(Matcher {
                prefix: "/api".to_string(),
                prefix_rewrite: "/v1".to_string(),
            }),
            ..Default::default()
        };

        let result = route.is_matched("/api/test", None).unwrap();
        assert_eq!(result, Some("/v1/test".to_string()));

        let result = route.is_matched("/other/test", None).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_route_host_matching() {
        let mut route = RouteConfig {
            host_name: Some("example.com".to_string()),
            matcher: Some(Matcher {
                prefix: "/api".to_string(),
                prefix_rewrite: "/v1".to_string(),
            }),
            ..Default::default()
        };

        let mut headers = HeaderMap::new();
        headers.insert("Host", HeaderValue::from_static("example.com"));

        let result = route.is_matched("/api/test", Some(&headers)).unwrap();
        assert_eq!(result, Some("/v1/test".to_string()));

        headers.insert("Host", HeaderValue::from_static("wrong.com"));
        let result = route.is_matched("/api/test", Some(&headers)).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_api_service_equality() {
        let service1 = ApiService {
            listen_port: 8080,
            ..Default::default()
        };

        let service2 = ApiService {
            listen_port: 8080,
            ..Default::default()
        };

        assert_eq!(service1, service2);
    }

    #[test]
    fn test_service_config_serialization() {
        let route = RouteConfig {
            router: Router::WeightBased(WeightBasedRoute {
                routes: vec![WeightedRouteItem {
                    weight: 1,
                    index: 0,
                    endpoint: "http://example.com".to_string(),
                    ..Default::default()
                }],
            }),
            ..Default::default()
        };

        let api_service = ApiService {
            listen_port: 8080,
            route_configs: vec![route],
            ..Default::default()
        };

        let mut app_config = AppConfig::default();
        let static_config = StaticConifg {
            access_log: Some("/var/log/proxy.log".to_string()),
            admin_port: Some(9090),
            ..Default::default()
        };
        app_config.static_config = static_config;
        let mut api_service = ApiService::default();
        api_service.listen_port = 8080;

        let header_based = WeightBasedRoute {
            routes: vec![WeightRoute {
                weight: 1,
                index: 0,
                base_route: BaseRoute {
                    endpoint: "http://www.baidu.com".to_string(),
                    ..Default::default()
                },
            }],
        };
        let route = Route {
            route_id: "test_route".to_string(),
            route_cluster: LoadbalancerStrategy::WeightBased(header_based),
            ..Default::default()
        };
        let service_config = ServiceConfig {
            server_type: ServiceType::Http,
            routes: vec![route],
            ..Default::default()
        };
        api_service.service_config = service_config;
        app_config.api_service_config.insert(8080, api_service);
        // 序列化为JSON
        let json_str = serde_yaml::to_string(&app_config).unwrap();
        println!("{}", json_str);
        println!("{}", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        // // 反序列化JSON
        // let deserialized_config: AppConfig = serde_json::from_str(&json_str).unwrap();

        // // 验证静态配置是否正确序列化和反序列化
        // assert_eq!(app_config, deserialized_config);
        // assert_eq!(deserialized_config.static_config.admin_port, Some(9090));
        // assert_eq!(
        //     deserialized_config.static_config.access_log,
        //     Some("/var/log/proxy.log".to_string())
        // );
    }
}
