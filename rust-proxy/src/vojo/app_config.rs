use super::allow_deny_ip::AllowResult;

use crate::constants::common_constants::DEFAULT_ADMIN_PORT;
use crate::constants::common_constants::DEFAULT_LOG_LEVEL;
use crate::utils::uuid::get_uuid;
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
use serde::Serializer;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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
    #[serde(
        rename = "services",
        deserialize_with = "deserialize_service_config",
        serialize_with = "serialize_api_service_config"
    )]
    pub api_service_config: HashMap<i32, ApiService>,
}
fn serialize_api_service_config<S>(
    config: &HashMap<i32, ApiService>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let vec: Vec<ApiService> = config.values().cloned().collect();
    vec.serialize(serializer)
}

fn deserialize_static_config<'de, D>(deserializer: D) -> Result<StaticConifg, D::Error>
where
    D: Deserializer<'de>,
{
    info!("deserialize_static_config");
    let mut static_config = StaticConifg::deserialize(deserializer)?;
    if static_config.health_check_log_enabled.is_none() {
        static_config.health_check_log_enabled = Some(false);
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
fn is_empty(value: &String) -> bool {
    value.is_empty()
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Route {
    #[serde(skip_serializing_if = "is_empty", default = "default_route_id")]
    pub route_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matcher: Option<Matcher>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_deny_list: Option<Vec<AllowDenyObject>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authentication: Option<Authentication>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ratelimit: Option<Ratelimit>,
    pub route_cluster: LoadbalancerStrategy,
}
fn default_route_id() -> String {
    get_uuid()
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
    pub fn is_allowed(
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
            is_allowed = !ratelimit_strategy.should_limit(header_map, ip)?;
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cert_str: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
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
    pub health_check_log_enabled: Option<bool>,
    pub database_url: Option<String>,
    pub admin_port: Option<i32>,
    pub config_file_path: Option<String>,
    pub log_level: Option<LogLevel>,
}
impl Default for StaticConifg {
    fn default() -> Self {
        Self {
            health_check_log_enabled: Some(false),
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

    use super::*;
    use crate::vojo::authentication::BasicAuth;
    use crate::vojo::health_check::BaseHealthCheckParam;
    use crate::vojo::health_check::HttpHealthCheckParam;
    use crate::vojo::route::BaseRoute;

    use crate::vojo::route::HeaderBasedRoute;
    use crate::vojo::route::HeaderRoute;
    use crate::vojo::route::LoadbalancerStrategy::WeightBased;

    use crate::vojo::route::PollBaseRoute;
    use crate::vojo::route::PollRoute;
    use crate::vojo::route::RandomBaseRoute;
    use crate::vojo::route::RandomRoute;
    use crate::vojo::route::TextMatch;
    use crate::vojo::route::WeightBasedRoute;
    use crate::vojo::route::WeightRoute;
    fn create_api_service1() -> ApiService {
        let mut api_service = ApiService {
            listen_port: 8080,
            ..Default::default()
        };

        let header_based = WeightBasedRoute {
            routes: vec![
                WeightRoute {
                    weight: 1,
                    index: 0,
                    base_route: BaseRoute {
                        endpoint: "http://127.0.0.1:9394".to_string(),
                        ..Default::default()
                    },
                },
                WeightRoute {
                    weight: 2,
                    index: 0,
                    base_route: BaseRoute {
                        endpoint: "http://127.0.0.1:9396".to_string(),
                        ..Default::default()
                    },
                },
                WeightRoute {
                    weight: 3,
                    index: 0,
                    base_route: BaseRoute {
                        endpoint: "http://127.0.0.1:9395".to_string(),
                        ..Default::default()
                    },
                },
            ],
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
        api_service
    }
    fn create_api_service2() -> ApiService {
        let mut api_service = ApiService {
            listen_port: 8080,
            ..Default::default()
        };

        let poll_route = PollRoute {
            routes: vec![
                PollBaseRoute {
                    base_route: BaseRoute {
                        endpoint: "http://127.0.0.1:9394".to_string(),
                        ..Default::default()
                    },
                },
                PollBaseRoute {
                    base_route: BaseRoute {
                        endpoint: "http://127.0.0.1:9395".to_string(),
                        ..Default::default()
                    },
                },
                PollBaseRoute {
                    base_route: BaseRoute {
                        endpoint: "http://127.0.0.1:9396".to_string(),
                        ..Default::default()
                    },
                },
            ],
            current_index: 0,
        };
        let route = Route {
            route_id: "test_route".to_string(),
            route_cluster: LoadbalancerStrategy::Poll(poll_route),
            ..Default::default()
        };
        let service_config = ServiceConfig {
            server_type: ServiceType::Http,
            routes: vec![route],
            ..Default::default()
        };
        api_service.service_config = service_config;
        api_service
    }
    fn create_api_service3() -> ApiService {
        let mut api_service = ApiService {
            listen_port: 8080,
            ..Default::default()
        };

        let poll_route = RandomRoute {
            routes: vec![
                RandomBaseRoute {
                    base_route: BaseRoute {
                        endpoint: "http://127.0.0.1:9394".to_string(),
                        ..Default::default()
                    },
                },
                RandomBaseRoute {
                    base_route: BaseRoute {
                        endpoint: "http://127.0.0.1:9395".to_string(),
                        ..Default::default()
                    },
                },
                RandomBaseRoute {
                    base_route: BaseRoute {
                        endpoint: "http://127.0.0.1:9396".to_string(),
                        ..Default::default()
                    },
                },
            ],
        };
        let route = Route {
            route_id: "test_route".to_string(),
            route_cluster: LoadbalancerStrategy::Random(poll_route),
            ..Default::default()
        };
        let service_config = ServiceConfig {
            server_type: ServiceType::Http,
            routes: vec![route],
            ..Default::default()
        };
        api_service.service_config = service_config;
        api_service
    }
    fn create_api_service4() -> ApiService {
        let mut api_service = ApiService {
            listen_port: 8080,
            ..Default::default()
        };

        let poll_route = HeaderBasedRoute {
            routes: vec![
                HeaderRoute {
                    base_route: BaseRoute {
                        endpoint: "http://127.0.0.1:9395".to_string(),
                        ..Default::default()
                    },
                    header_key: "test".to_string(),
                    header_value_mapping_type: crate::vojo::route::HeaderValueMappingType::Text(
                        TextMatch {
                            value: "test".to_string(),
                        },
                    ),
                },
                HeaderRoute {
                    base_route: BaseRoute {
                        endpoint: "http://127.0.0.1:9396".to_string(),
                        ..Default::default()
                    },
                    header_key: "test".to_string(),
                    header_value_mapping_type: crate::vojo::route::HeaderValueMappingType::Text(
                        TextMatch {
                            value: "test".to_string(),
                        },
                    ),
                },
                HeaderRoute {
                    base_route: BaseRoute {
                        endpoint: "http://127.0.0.1:9397".to_string(),
                        ..Default::default()
                    },
                    header_key: "test".to_string(),
                    header_value_mapping_type: crate::vojo::route::HeaderValueMappingType::Text(
                        TextMatch {
                            value: "test".to_string(),
                        },
                    ),
                },
            ],
        };
        let route = Route {
            route_id: "test_route".to_string(),
            route_cluster: LoadbalancerStrategy::HeaderBased(poll_route),
            ..Default::default()
        };
        let service_config = ServiceConfig {
            server_type: ServiceType::Http,
            routes: vec![route],
            ..Default::default()
        };
        api_service.service_config = service_config;
        api_service
    }
    pub fn create_default_app_config() -> AppConfig {
        let mut app_config = AppConfig::default();
        let static_config = StaticConifg {
            health_check_log_enabled: Some(false),
            admin_port: Some(9090),
            ..Default::default()
        };
        app_config.static_config = static_config;
        let mut api_service = ApiService {
            listen_port: 8080,
            ..Default::default()
        };

        let header_based = WeightBasedRoute {
            routes: vec![WeightRoute {
                weight: 1,
                index: 0,
                base_route: BaseRoute {
                    endpoint: "http://127.0.0.1:9393".to_string(),
                    ..Default::default()
                },
            }],
        };
        let route = Route {
            route_id: "test_route".to_string(),
            route_cluster: LoadbalancerStrategy::WeightBased(header_based),
            health_check: Some(HealthCheckType::HttpGet(HttpHealthCheckParam {
                path: "/health".to_string(),
                base_health_check_param: BaseHealthCheckParam {
                    interval: 5,
                    timeout: 5,
                },
            })),

            liveness_config: Some(LivenessConfig {
                min_liveness_count: 1,
            }),
            ..Default::default()
        };
        let service_config = ServiceConfig {
            server_type: ServiceType::Http,
            routes: vec![route],
            ..Default::default()
        };
        api_service.service_config = service_config;
        app_config
            .api_service_config
            .insert(8080, create_api_service1());
        app_config
            .api_service_config
            .insert(8081, create_api_service2());
        app_config
            .api_service_config
            .insert(8082, create_api_service3());
        app_config
            .api_service_config
            .insert(8083, create_api_service4());
        app_config
    }
    #[test]
    fn test_app_config_serialize_with_static_config() {
        let app_config = create_default_app_config();
        // 序列化为JSON
        let json_str = serde_yaml::to_string(&app_config).unwrap();
        println!("{}", json_str);
    }

    use crate::vojo::allow_deny_ip::AllowDenyObject;
    use crate::vojo::authentication::Authentication;
    use http::HeaderValue;

    #[test]
    fn test_static_config_default() {
        let config = StaticConifg::default();
        assert_eq!(config.health_check_log_enabled, Some(false));
        assert_eq!(config.database_url, Some("".to_string()));
        assert_eq!(config.admin_port, Some(DEFAULT_ADMIN_PORT));
        assert_eq!(config.config_file_path, Some("".to_string()));
        assert_eq!(config.log_level, Some(DEFAULT_LOG_LEVEL));
    }

    #[test]
    fn test_route_matching() {
        let route = Route {
            matcher: Some(Matcher {
                prefix: "/api".to_string(),
                prefix_rewrite: "/v1".to_string(),
            }),
            ..Default::default()
        };

        let result = route.is_matched("/api/test".to_string(), None).unwrap();
        assert_eq!(result, Some("/v1/test".to_string()));

        let result = route.is_matched("/other/test".to_string(), None).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_route_host_matching() {
        let route = Route {
            host_name: Some("example.com".to_string()),
            matcher: Some(Matcher {
                prefix: "/api".to_string(),
                prefix_rewrite: "/v1".to_string(),
            }),
            ..Default::default()
        };

        let mut headers = HeaderMap::new();
        headers.insert("Host", HeaderValue::from_static("example.com"));

        let result = route
            .is_matched("/api/test".to_string(), Some(headers.clone()))
            .unwrap();
        assert_eq!(result, Some("/v1/test".to_string()));

        headers.insert("Host", HeaderValue::from_static("wrong.com"));
        let result = route
            .is_matched("/api/test".to_string(), Some(headers))
            .unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_ip_allowing() {
        let allow_obj = AllowDenyObject {
            limit_type: crate::vojo::allow_deny_ip::AllowType::AllowAll,
            value: Some("IP_ADDRESS".to_string()),
        };

        let route = Route {
            allow_deny_list: Some(vec![allow_obj]),
            ..Default::default()
        };

        let allowed = route.is_allowed("192.168.1.1".to_string(), None).unwrap();
        assert!(allowed);

        let allowed = route.is_allowed("192.168.1.2".to_string(), None).unwrap();
        assert!(allowed);
    }

    #[test]
    fn test_authentication() {
        let auth = Authentication::Basic(BasicAuth {
            credentials: "test:test".to_string(),
        });
        let route = Route {
            authentication: Some(auth),
            ..Default::default()
        };

        let mut headers = HeaderMap::new();
        headers.insert(
            "Authorization",
            HeaderValue::from_static("Basic dGVzdDp0ZXN0"),
        );

        let allowed = route
            .is_allowed("192.168.1.1".to_string(), Some(headers))
            .unwrap();
        assert!(allowed);

        let mut wrong_headers = HeaderMap::new();
        wrong_headers.insert("Authorization", HeaderValue::from_static("Basic wrong"));

        let allowed = route
            .is_allowed("192.168.1.1".to_string(), Some(wrong_headers))
            .unwrap();
        assert!(!allowed);
    }

    #[test]
    fn test_api_service_equality() {
        let service1 = ApiService {
            listen_port: 8080,
            api_service_id: "id1".to_string(),
            ..Default::default()
        };

        let service2 = ApiService {
            listen_port: 8080,
            api_service_id: "id1".to_string(),
            ..Default::default()
        };

        let service3 = ApiService {
            listen_port: 8081,
            api_service_id: "id1".to_string(),
            ..Default::default()
        };

        assert_eq!(service1, service2);
        assert_ne!(service1, service3);
    }

    #[test]
    fn test_service_config_serialization() {
        let route = Route {
            route_cluster: WeightBased(WeightBasedRoute {
                routes: vec![WeightRoute {
                    weight: 1,
                    index: 0,
                    base_route: BaseRoute {
                        endpoint: "http://example.com".to_string(),
                        ..Default::default()
                    },
                }],
            }),
            ..Default::default()
        };

        let service_config = ServiceConfig {
            server_type: ServiceType::Http,
            routes: vec![route],
            ..Default::default()
        };

        let api_service = ApiService {
            listen_port: 8080,
            service_config,
            ..Default::default()
        };

        let mut app_config = AppConfig::default();
        app_config.api_service_config.insert(8080, api_service);

        let yaml = serde_yaml::to_string(&app_config).unwrap();
        assert!(yaml.contains("listen_port: 8080"));
        assert!(yaml.contains("server_type: Http"));
    }

    #[test]
    fn test_log_level_conversion() {
        let config = StaticConifg {
            log_level: Some(LogLevel::Debug),
            ..Default::default()
        };
        assert_eq!(config.get_log_level(), LevelFilter::DEBUG);

        let config = StaticConifg {
            log_level: Some(LogLevel::Info),
            ..Default::default()
        };
        assert_eq!(config.get_log_level(), LevelFilter::INFO);

        let config = StaticConifg {
            log_level: Some(LogLevel::Error),
            ..Default::default()
        };
        assert_eq!(config.get_log_level(), LevelFilter::ERROR);

        let config = StaticConifg {
            log_level: Some(LogLevel::Warn),
            ..Default::default()
        };
        assert_eq!(config.get_log_level(), LevelFilter::WARN);

        let config = StaticConifg {
            log_level: None,
            ..Default::default()
        };
        assert_eq!(config.get_log_level(), LevelFilter::INFO);
    }
}
