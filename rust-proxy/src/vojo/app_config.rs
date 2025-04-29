use super::allow_deny_ip::AllowResult;
use super::app_config_vistor::ApiServiceVistor;
use super::app_config_vistor::ServiceConfigVistor;
use crate::vojo::allow_deny_ip::AllowDenyObject;
use crate::vojo::anomaly_detection::AnomalyDetectionType;
use crate::vojo::app_config_vistor::from_loadbalancer_strategy_vistor;
use crate::vojo::app_config_vistor::RouteVistor;
use crate::vojo::app_error::AppError;
use crate::vojo::authentication::Authentication;
use crate::vojo::health_check::HealthCheckType;
use crate::vojo::rate_limit::Ratelimit;
use crate::vojo::route::LoadbalancerStrategy;
use http::HeaderMap;
use http::HeaderValue;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::RwLock;
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
    pub route_id: String,
    pub host_name: Option<String>,
    pub matcher: Option<Matcher>,
    pub allow_deny_list: Option<Vec<AllowDenyObject>>,
    pub authentication: Option<Authentication>,
    pub anomaly_detection: Option<AnomalyDetectionType>,
    pub liveness_status: LivenessStatus,
    pub rewrite_headers: Option<HashMap<String, String>>,
    pub liveness_config: Option<LivenessConfig>,
    pub health_check: Option<HealthCheckType>,
    pub ratelimit: Option<Ratelimit>,
    pub route_cluster: LoadbalancerStrategy,
}
impl Route {
    pub async fn from(route_vistor: RouteVistor) -> Result<Route, AppError> {
        let cloned_cluster = route_vistor.route_cluster.clone();
        let new_matcher = route_vistor.matcher.clone().map(|mut item| {
            let src_prefix = item.prefix.clone();
            if !src_prefix.ends_with('/') {
                let src_prefix_len = item.prefix.len();
                item.prefix.insert(src_prefix_len, '/');
            }
            if !src_prefix.starts_with('/') {
                item.prefix.insert(0, '/')
            }
            let path_rewrite = item.prefix_rewrite.clone();

            if !path_rewrite.starts_with('/') {
                item.prefix_rewrite.insert(0, '/')
            }
            item
        });

        let count = cloned_cluster.get_routes_len() as i32;

        Ok(Route {
            route_id: route_vistor.route_id,
            host_name: route_vistor.host_name,
            matcher: new_matcher,
            allow_deny_list: route_vistor.allow_deny_list,
            authentication: route_vistor.authentication,
            anomaly_detection: route_vistor.anomaly_detection,
            liveness_status: Arc::new(RwLock::new(LivenessStatus {
                current_liveness_count: count,
            })),
            rewrite_headers: route_vistor.rewrite_headers,
            liveness_config: route_vistor.liveness_config,
            health_check: route_vistor.health_check,
            ratelimit: route_vistor.ratelimit,
            route_cluster: from_loadbalancer_strategy_vistor(route_vistor.route_cluster),
        })
    }
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
impl ServiceConfig {
    pub async fn from(service_config_vistor: ServiceConfigVistor) -> Result<Self, AppError> {
        let mut routes = vec![];
        for item in service_config_vistor.routes {
            routes.push(Route::from(item).await?)
        }
        Ok(ServiceConfig {
            server_type: service_config_vistor.server_type,
            cert_str: service_config_vistor.cert_str,
            key_str: service_config_vistor.key_str,
            routes,
        })
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ApiService {
    pub listen_port: i32,
    pub api_service_id: String,
    pub service_config: ServiceConfig,
    #[serde(skip_deserializing, skip_serializing)]
    pub sender: mpsc::Sender<()>,
}
impl ApiService {
    pub async fn from(api_service_vistor: ApiServiceVistor) -> Result<Self, AppError> {
        let api_service_config = ServiceConfig::from(api_service_vistor.service_config).await?;
        Ok(ApiService {
            listen_port: api_service_vistor.listen_port,
            api_service_id: api_service_vistor.api_service_id,
            service_config: api_service_config,
        })
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct StaticConifg {
    pub access_log: Option<String>,
    pub database_url: Option<String>,
    pub admin_port: i32,
    pub config_file_path: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AppConfig {
    pub static_config: StaticConifg,
    pub api_service_config: Vec<ApiService>,
}
