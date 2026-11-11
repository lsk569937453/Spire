use crate::constants::common_constants::DEFAULT_ADMIN_PORT;
use crate::middleware::middlewares::CheckResult;
use crate::middleware::middlewares::MiddleWares;
use crate::middleware::middlewares::Middleware;
use crate::utils::uuid::get_uuid;
use crate::vojo::anomaly_detection::AnomalyDetectionType;
use crate::vojo::app_error::AppError;
use crate::vojo::concurrency::ConcurrencyConfig;
use crate::vojo::health_check::HealthCheckType;
use crate::vojo::matcher::MatcherRule;
use crate::vojo::router::Router;
use crate::vojo::router::deserialize_router;
use crate::vojo::timeout_config::TimeoutConfig;
use http::HeaderMap;
use http::HeaderValue;
use http::Method;
use ipnet::IpNet;
use serde::Deserializer;
use serde::Serializer;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::OnceLock;
use tokio::sync::mpsc;
use tracing_subscriber::filter::LevelFilter;
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub acme: AcmeConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health_check_log_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub admin_port: Option<i32>,
    /// Address the control plane binds to. Defaults to loopback because the
    /// admin API can read and rewrite the whole configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin_listen: Option<String>,
    /// Bearer token required by the control plane. Strongly recommended
    /// whenever `admin_listen` is not a loopback address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin_token: Option<String>,
    /// CIDR ranges of reverse proxies / load balancers whose
    /// `X-Forwarded-For` / `X-Real-IP` headers are trusted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trusted_proxies: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_level: Option<LogLevel>,
    /// Upstream request timeout in **milliseconds** (covers connect + send +
    /// time to first response byte). The historical name was
    /// `upstream_timeout_secs`, but the value was always interpreted as
    /// milliseconds; the alias keeps old configurations loading.
    #[serde(
        skip_serializing_if = "Option::is_none",
        default = "default_upstream_timeout",
        alias = "upstream_timeout_secs"
    )]
    pub upstream_timeout_ms: Option<i32>,
    /// Global fallback for the per-endpoint concurrency limit (see the
    /// route-level `concurrency` config). Absent (`null`) means "derive it
    /// at startup from the detected container memory limit"; an explicit
    /// value always wins. The derived value is deliberately not stored back
    /// into this field so reloads and persistence keep the operator's intent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_concurrency_limit: Option<i32>,
    #[serde(
        rename = "servers",
        deserialize_with = "deserialize_service_config",
        serialize_with = "serialize_api_service_config"
    )]
    pub api_service_config: HashMap<i32, ApiService>,
    /// Parsed form of `trusted_proxies`, built once per configuration.
    #[serde(skip)]
    pub(crate) trusted_proxy_nets: OnceLock<Vec<IpNet>>,
}

/// Never print the admin token into logs.
impl core::fmt::Debug for AppConfig {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AppConfig")
            .field("acme", &self.acme)
            .field("health_check_log_enabled", &self.health_check_log_enabled)
            .field("admin_port", &self.admin_port)
            .field("admin_listen", &self.admin_listen)
            .field(
                "admin_token",
                &self.admin_token.as_ref().map(|_| "<redacted>"),
            )
            .field("trusted_proxies", &self.trusted_proxies)
            .field("log_level", &self.log_level)
            .field("upstream_timeout_ms", &self.upstream_timeout_ms)
            .field(
                "upstream_concurrency_limit",
                &self.upstream_concurrency_limit,
            )
            .field("servers", &self.api_service_config)
            .finish()
    }
}

fn default_upstream_timeout() -> Option<i32> {
    Some(5000)
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            health_check_log_enabled: Some(false),
            admin_port: Some(DEFAULT_ADMIN_PORT),
            admin_listen: None,
            admin_token: None,
            trusted_proxies: None,
            log_level: None,
            api_service_config: Default::default(),
            acme: AcmeConfig::default(),
            upstream_timeout_ms: Some(5000),
            // None = derive from the detected memory limit at startup.
            upstream_concurrency_limit: None,
            trusted_proxy_nets: OnceLock::new(),
        }
    }
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transcode {
    pub proto_descriptor_set: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RouteConfig {
    #[serde(skip_serializing_if = "is_empty", default = "default_route_id")]
    pub route_id: String,

    #[serde(default)]
    pub matchers: Vec<MatcherRule>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_rewrite: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcode: Option<Transcode>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<TimeoutConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<ConcurrencyConfig>,
}

fn default_route_id() -> String {
    get_uuid()
}

impl RouteConfig {
    pub fn match_and_rewrite(
        &self,
        path: &str,
        method: &Method,
        headers: &HeaderMap<HeaderValue>,
    ) -> Result<Option<String>, AppError> {
        if !self
            .matchers
            .iter()
            .all(|rule| rule.matches(method, path, headers))
        {
            return Ok(None);
        }
        if let Some(rewrite_template) = &self.path_rewrite {
            // Reuse the regex the path matcher has already compiled instead of
            // compiling the same pattern again on every request.
            if let Some(re) = self.matchers.iter().find_map(|m| m.path_regex()) {
                let rewritten_path = re.replace(path, rewrite_template.as_str());
                return Ok(Some(rewritten_path.into_owned()));
            }
            if let Some(pattern) = self.matchers.iter().find_map(|m| match m {
                MatcherRule::Path { value, .. } => Some(value),
                _ => None,
            }) && let Some(stripped_path) = path.strip_prefix(pattern.as_str())
            {
                return Ok(Some(format!("{rewrite_template}{stripped_path}")));
            }

            Ok(Some(path.to_string()))
        } else {
            Ok(Some(path.to_string()))
        }
    }

    pub fn is_allowed(
        &self,
        peer_addr: &SocketAddr,
        headers_option: Option<&HeaderMap<HeaderValue>>,
    ) -> Result<CheckResult, AppError> {
        if let Some(middlewares) = &self.middlewares {
            for middleware in middlewares.iter() {
                let check_res = middleware.check_request(peer_addr, headers_option)?;
                if !check_res.is_allowed() {
                    return Ok(check_res);
                }
            }
        }
        Ok(CheckResult::Allowed)
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
    /// HTTP/2 (h2c) and gRPC.
    ///
    /// The protocol name `tls` is kept as an accepted alias because that is what
    /// older configurations used, but `http2` is the documented name.
    #[serde(rename = "http2", alias = "tls")]
    Http2,
    #[serde(rename = "http2tls", alias = "Http2Tls")]
    Http2Tls,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiService {
    #[serde(rename = "listen")]
    pub listen_port: i32,

    #[serde(rename = "protocol")]
    pub server_type: ServiceType,
    #[serde(rename = "domains")]
    pub domain_config: Vec<String>,
    #[serde(rename = "routes")]
    pub route_configs: Vec<RouteConfig>,
    #[serde(skip_deserializing, skip_serializing)]
    pub sender: mpsc::Sender<()>,
}
#[derive(Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "PascalCase")]
pub enum AcmeConfig {
    #[default]
    LetsEncrypt,
    #[serde(rename = "zerossl")]
    ZeroSsl {
        eab_kid: String,
        eab_hmac_key: String,
    },
}

/// Never print the ACME EAB key into logs.
impl core::fmt::Debug for AcmeConfig {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            AcmeConfig::LetsEncrypt => f.write_str("LetsEncrypt"),
            AcmeConfig::ZeroSsl { eab_kid, .. } => f
                .debug_struct("ZeroSsl")
                .field("eab_kid", eab_kid)
                .field("eab_hmac_key", &"<redacted>")
                .finish(),
        }
    }
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

            #[serde(rename = "domains")]
            #[serde(default)]
            pub domain_config: Vec<String>,
            #[serde(rename = "routes")]
            pub route_configs: Vec<RouteConfig>,
        }

        let api_service_without_sender = ApiServiceWithoutSender::deserialize(deserializer)?;
        let (sender, _) = mpsc::channel(1);

        Ok(ApiService {
            listen_port: api_service_without_sender.port,
            server_type: api_service_without_sender.server_type,
            domain_config: api_service_without_sender.domain_config,
            route_configs: api_service_without_sender.route_configs,
            sender,
        })
    }
}
impl PartialEq for ApiService {
    fn eq(&self, other: &Self) -> bool {
        self.listen_port == other.listen_port
            && self.server_type == other.server_type
            && self.route_configs == other.route_configs
    }
}
impl Default for ApiService {
    fn default() -> Self {
        let (sender, _) = mpsc::channel(1); // Buffer size 1

        Self {
            listen_port: Default::default(),
            server_type: Default::default(),
            domain_config: Default::default(),
            route_configs: Default::default(),
            sender,
        }
    }
}

impl AppConfig {
    pub fn get_log_level(&self) -> LevelFilter {
        match self.log_level {
            Some(LogLevel::Debug) => LevelFilter::DEBUG,
            Some(LogLevel::Info) => LevelFilter::INFO,
            Some(LogLevel::Error) => LevelFilter::ERROR,
            Some(LogLevel::Warn) => LevelFilter::WARN,
            Some(LogLevel::Trace) => LevelFilter::TRACE,

            None => LevelFilter::OFF,
        }
    }

    /// Parsed `trusted_proxies` CIDRs (cached: parsing them per request would be
    /// wasteful, and an invalid entry must not be able to panic the proxy).
    pub fn trusted_proxy_nets(&self) -> &[IpNet] {
        self.trusted_proxy_nets.get_or_init(|| {
            let mut nets = Vec::new();
            for entry in self.trusted_proxies.iter().flatten() {
                match entry.parse::<IpNet>() {
                    Ok(net) => nets.push(net),
                    Err(e) => error!("Invalid trusted_proxies entry '{entry}': {e}"),
                }
            }
            nets
        })
    }

    pub fn admin_listen_addr(&self) -> String {
        self.admin_listen
            .clone()
            .unwrap_or_else(|| crate::constants::common_constants::DEFAULT_ADMIN_LISTEN.to_string())
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
    #[serde(rename = "trace")]
    Trace,
}

#[cfg(test)]
mod tests {

    use std::sync::atomic::{AtomicI32, AtomicUsize};

    use super::*;
    use crate::middleware::authentication::ApiKeyAuth;

    use crate::middleware::cors_config::CorsAllowedOrigins;

    use crate::middleware::rate_limit::IPBasedRatelimit;
    use crate::middleware::rate_limit::TimeUnit;
    use crate::middleware::rate_limit::TokenBucketRateLimit;
    use crate::vojo::health_check::BaseHealthCheckParam;
    use crate::vojo::health_check::HttpHealthCheckParam;
    use crate::vojo::router::BaseRoute;
    use crate::vojo::router::WeightedRouteItem;

    use crate::middleware::allow_deny_ip::AllowType;
    use crate::middleware::allow_deny_ip::{AllowDenyIp, AllowDenyItem};
    use crate::middleware::authentication::Authentication;
    use crate::middleware::cors_config::CorsAllowHeader;
    use crate::middleware::cors_config::CorsConfig;
    use crate::middleware::cors_config::Method;
    use crate::middleware::rate_limit::LimitLocation;
    use crate::middleware::rate_limit::Ratelimit;
    use crate::vojo::router::HeaderBasedRoute;
    use crate::vojo::router::HeaderRoutingRule;
    use crate::vojo::router::PollRoute;
    use crate::vojo::router::RandomRoute;
    use std::sync::{Arc, Mutex};

    use crate::vojo::router::WeightBasedRoute;
    use http::HeaderValue;
    fn create_api_service1() -> ApiService {
        let mut api_service = ApiService {
            listen_port: 8081,
            ..Default::default()
        };

        let header_based = WeightBasedRoute {
            routes: vec![
                WeightedRouteItem {
                    weight: 1,
                    index: AtomicI32::new(0),
                    endpoint: "http://127.0.0.1:9394".to_string(),
                    ..Default::default()
                },
                WeightedRouteItem {
                    weight: 2,
                    index: AtomicI32::new(0),
                    endpoint: "http://127.0.0.1:9396".to_string(),
                    ..Default::default()
                },
                WeightedRouteItem {
                    weight: 3,
                    index: AtomicI32::new(0),
                    endpoint: "http://127.0.0.1:9395".to_string(),
                    ..Default::default()
                },
            ],
        };
        let route = RouteConfig {
            route_id: "test_route".to_string(),
            router: Router::WeightBased(header_based),
            ..Default::default()
        };

        api_service.route_configs = vec![route];
        api_service
    }
    fn create_api_service2() -> ApiService {
        let mut api_service = ApiService {
            listen_port: 8082,
            ..Default::default()
        };

        let poll_route = PollRoute {
            routes: vec![
                BaseRoute {
                    endpoint: "http://127.0.0.1:9394".to_string(),
                    ..Default::default()
                },
                BaseRoute {
                    endpoint: "http://127.0.0.1:9395".to_string(),
                    ..Default::default()
                },
                BaseRoute {
                    endpoint: "http://127.0.0.1:9396".to_string(),
                    ..Default::default()
                },
            ],
            current_index: AtomicUsize::new(0),
        };
        let route = RouteConfig {
            route_id: "test_route".to_string(),
            router: Router::Poll(poll_route),

            ..Default::default()
        };

        api_service.route_configs = vec![route];
        api_service
    }
    fn create_api_service3() -> ApiService {
        let mut api_service = ApiService {
            listen_port: 8083,
            ..Default::default()
        };

        let poll_route = RandomRoute {
            routes: vec![
                BaseRoute {
                    endpoint: "http://127.0.0.1:9394".to_string(),
                    ..Default::default()
                },
                BaseRoute {
                    endpoint: "http://127.0.0.1:9395".to_string(),
                    ..Default::default()
                },
                BaseRoute {
                    endpoint: "http://127.0.0.1:9396".to_string(),
                    ..Default::default()
                },
            ],
        };
        let route = RouteConfig {
            route_id: "test_route".to_string(),
            router: Router::Random(poll_route),

            ..Default::default()
        };

        api_service.route_configs = vec![route];
        api_service
    }
    fn create_api_service4() -> ApiService {
        let mut api_service = ApiService {
            listen_port: 8084,
            ..Default::default()
        };

        let poll_route =
            HeaderBasedRoute {
                routes: vec![
                    HeaderRoutingRule {
                        match_regex: Default::default(),
                        header_key: "test".to_string(),
                        header_value_mapping_type:
                            crate::vojo::router::HeaderValueMappingType::Text("test".to_string()),
                        endpoint: "http://127.0.0.1:9395".to_string(),
                        ..Default::default()
                    },
                    HeaderRoutingRule {
                        match_regex: Default::default(),
                        header_key: "test".to_string(),
                        header_value_mapping_type:
                            crate::vojo::router::HeaderValueMappingType::Text("test".to_string()),
                        endpoint: "http://127.0.0.1:9396".to_string(),
                        ..Default::default()
                    },
                    HeaderRoutingRule {
                        match_regex: Default::default(),
                        header_key: "test".to_string(),
                        header_value_mapping_type:
                            crate::vojo::router::HeaderValueMappingType::Text("test".to_string()),
                        endpoint: "http://127.0.0.1:9397".to_string(),
                        ..Default::default()
                    },
                ],
            };
        let route = RouteConfig {
            route_id: "test_route".to_string(),
            router: Router::HeaderBased(poll_route),
            ..Default::default()
        };

        api_service.route_configs = vec![route];
        api_service
    }
    pub fn create_default_app_config() -> AppConfig {
        let mut app_config = AppConfig::default();

        let mut api_service = ApiService {
            listen_port: 8085,
            ..Default::default()
        };

        let header_based = WeightBasedRoute {
            routes: vec![WeightedRouteItem {
                weight: 1,
                index: AtomicI32::new(0),
                endpoint: "http://127.0.0.1:9393".to_string(),
                ..Default::default()
            }],
        };
        let route = RouteConfig {
            route_id: "test_route".to_string(),
            matchers: vec![MatcherRule::Path {
                value: "/".to_string(),
                match_type: crate::vojo::matcher::PathMatchType::Exact,
                regex: Default::default(),
            }],
            router: Router::WeightBased(header_based),

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
            middlewares: Some(vec![
                MiddleWares::Authentication(Authentication::ApiKey(ApiKeyAuth {
                    key: "test".to_string(),
                    value: "test".to_string(),
                })),
                MiddleWares::RateLimit(Arc::new(Mutex::new(Ratelimit::TokenBucket(
                    TokenBucketRateLimit {
                        capacity: 10,
                        rate_per_unit: 10,
                        scope: LimitLocation::IP(IPBasedRatelimit {
                            value: "192.168.0.1".to_string(),
                        }),
                        unit: TimeUnit::Second,
                        ..Default::default()
                    },
                )))),
                MiddleWares::AllowDenyList(AllowDenyIp {
                    rules: vec![AllowDenyItem {
                        value: Some("192.168.0.2".to_string()),
                        policy: AllowType::AllowAll,
                    }],
                }),
                MiddleWares::Cors(CorsConfig {
                    allow_credentials: Some(true),
                    allowed_origins: CorsAllowedOrigins::All,
                    allowed_methods: vec![Method::Get, Method::Post],
                    allowed_headers: Some(CorsAllowHeader::All),

                    max_age: Some(3600),
                    options_passthrough: Some(true),
                    ..Default::default()
                }),
            ]),
            ..Default::default()
        };

        api_service.route_configs = vec![route];
        app_config.api_service_config.insert(8079, api_service);

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
        let json_str = serde_yaml::to_string(&app_config).unwrap();
        println!("{json_str}");
    }
    #[test]
    fn test_static_config_default() {
        let config = AppConfig::default();
        assert_eq!(config.health_check_log_enabled, Some(false));
        assert_eq!(config.admin_port, Some(DEFAULT_ADMIN_PORT));
        assert_eq!(config.log_level, None);
    }
    use crate::constants::common_constants::DEFAULT_ADMIN_PORT;

    #[test]
    fn test_route_matching() {
        let route = RouteConfig {
            matchers: vec![MatcherRule::Path {
                value: "/api/test".to_string(),
                match_type: crate::vojo::matcher::PathMatchType::Exact,
                regex: Default::default(),
            }],
            path_rewrite: Some("/v1/test".to_string()),
            ..Default::default()
        };
        let get = http::Method::GET;
        let result = route
            .match_and_rewrite("/api/test", &get, &HeaderMap::new())
            .unwrap();
        assert_eq!(result, Some("/v1/test".to_string()));

        let result = route
            .match_and_rewrite("/other/test", &get, &HeaderMap::new())
            .unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_route_host_matching() {
        let route = RouteConfig {
            matchers: vec![
                MatcherRule::Path {
                    value: "/api/test".to_string(),
                    match_type: crate::vojo::matcher::PathMatchType::Exact,
                    regex: Default::default(),
                },
                MatcherRule::Host {
                    value: "example.com".to_string(),
                    regex: Default::default(),
                },
            ],
            path_rewrite: Some("/v1/test".to_string()),

            ..Default::default()
        };

        let mut headers = HeaderMap::new();
        headers.insert("Host", HeaderValue::from_static("example.com"));

        let result = route
            .match_and_rewrite("/api/test", &http::Method::GET, &headers)
            .unwrap();
        assert_eq!(result, Some("/v1/test".to_string()));

        headers.insert("Host", HeaderValue::from_static("wrong.com"));
        let result = route
            .match_and_rewrite("/api/test", &http::Method::GET, &headers)
            .unwrap();
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
                    index: AtomicI32::new(0),
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
        app_config.api_service_config.insert(8080, api_service);

        let yaml = serde_yaml::to_string(&app_config).unwrap();
        assert!(yaml.contains("listen: 8080"));
        assert!(yaml.contains("protocol: http"));
    }

    use crate::vojo::base_response::BaseResponse;
    #[tokio::test]
    async fn test_health_check() {
        let src = r#"
response_code: 0
response_object:
  servers:
  - listen: 8080
    protocol: http
    routes:
      - route_id: route1
        forward_to:
          kind: poll
          targets: []"#;
        let _: BaseResponse<AppConfig> = serde_yaml::from_str(src).unwrap();
    }

    #[test]
    fn legacy_upstream_timeout_secs_key_still_loads() {
        // Old configurations used the misleading name; the alias must keep
        // them loading with the value interpreted (as always) as milliseconds.
        let legacy = r#"
upstream_timeout_secs: 30000
servers: []"#;
        let config: AppConfig = serde_yaml::from_str(legacy).unwrap();
        assert_eq!(config.upstream_timeout_ms, Some(30000));

        // New name, same semantics; serializing always emits the new key.
        let config: AppConfig = serde_yaml::from_str("upstream_timeout_ms: 2500\nservers: []")
            .unwrap();
        assert_eq!(config.upstream_timeout_ms, Some(2500));
        let yaml = serde_yaml::to_string(&config).unwrap();
        assert!(yaml.contains("upstream_timeout_ms: 2500"));
        assert!(!yaml.contains("upstream_timeout_secs"));
    }

    #[test]
    fn route_concurrency_config_deserializes() {
        let src = r#"
upstream_concurrency_limit: 500
servers:
  - listen: 8080
    protocol: http
    routes:
      - route_id: limited
        forward_to: http://127.0.0.1:9394
        concurrency:
          limit: 200
          queue_length: 10
          queue_timeout_ms: 50"#;
        let config: AppConfig = serde_yaml::from_str(src).unwrap();
        assert_eq!(config.upstream_concurrency_limit, Some(500));
        let route = &config.api_service_config[&8080].route_configs[0];
        let concurrency = route.concurrency.as_ref().unwrap();
        assert_eq!(concurrency.limit, 200);
        assert_eq!(concurrency.queue_length, 10);
        assert_eq!(concurrency.queue_timeout_ms, 50);

        // Absent key = None = "derive from the detected memory limit at
        // startup" (and must not be persisted back once derived).
        let derived: AppConfig = serde_yaml::from_str("servers: []").unwrap();
        assert_eq!(derived.upstream_concurrency_limit, None);
    }
}
