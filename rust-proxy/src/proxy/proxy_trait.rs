use crate::vojo::app_config::Route;
use crate::vojo::app_error::AppError;
use crate::vojo::cors_config::CorsConfig;
use crate::vojo::route::BaseRoute;
use crate::SharedConfig;
use bytes::Bytes;
use http::header;
use http::header::HeaderMap;
use http::HeaderValue;
use http_body_util::combinators::BoxBody;
use hyper::Response;
use hyper::Uri;
use serde::Deserialize;
use serde::Serialize;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::Path;
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpireContext {
    pub port: i32,
    pub route: Option<Route>,
}
impl SpireContext {
    pub fn new(port: i32, route: Option<Route>) -> Self {
        Self { port, route }
    }
}
pub trait ChainTrait {
    async fn handle_before_request(
        &self,
        shared_config: SharedConfig,
        port: i32,
        mapping_key: String,
        headers: &HeaderMap,
        uri: Uri,
        peer_addr: SocketAddr,
        spire_context: &mut SpireContext,
    ) -> Result<Option<CheckResult>, AppError>;
    async fn handle_after_request(
        &self,
        cores_config: CorsConfig,
        response: &mut Response<BoxBody<Bytes, Infallible>>,
    ) -> Result<(), AppError>;
}
pub struct CommonCheckRequest;

#[derive(Debug, Clone)]
pub struct CheckResult {
    pub request_path: String,
    pub base_route: BaseRoute,
}

impl ChainTrait for CommonCheckRequest {
    async fn handle_after_request(
        &self,
        cors_config: CorsConfig,

        response: &mut Response<BoxBody<Bytes, Infallible>>,
    ) -> Result<(), AppError> {
        let headers = response.headers_mut();
        let origin = cors_config.allowed_origins.to_string();
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_str(origin.as_str()).map_err(|_| "HeaderValue is none")?,
        );

        let methods = cors_config
            .allowed_methods
            .iter()
            .map(|m| m.as_str().to_uppercase())
            .collect::<Vec<String>>()
            .join(", ");
        info!("methods: {}", methods);
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_str(&methods).map_err(|_| "Invalid header")?,
        );
        if let Some(cors_headers) = cors_config.allowed_headers {
            headers.insert(
                header::ACCESS_CONTROL_ALLOW_HEADERS,
                HeaderValue::from_str(&cors_headers.to_string()).map_err(|_| "Invalid header")?,
            );
        }
        if let Some(allow_credentials) = cors_config.allow_credentials {
            if allow_credentials {
                headers.insert(
                    header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
                    HeaderValue::from_static("true"),
                );
            }
        }
        if let Some(max_age) = cors_config.max_age {
            if max_age > 0 {
                headers.insert(
                    header::ACCESS_CONTROL_MAX_AGE,
                    HeaderValue::from_str(&max_age.to_string()).map_err(|_| "Invalid header")?,
                );
            }
        }

        if !cors_config.allowed_origins.is_all() {
            headers.append(header::VARY, HeaderValue::from_static("Origin"));
        }

        Ok(())
    }
    async fn handle_before_request(
        &self,
        shared_config: SharedConfig,
        port: i32,
        _mapping_key: String,
        headers: &HeaderMap,
        uri: Uri,
        peer_addr: SocketAddr,
        spire_context: &mut SpireContext,
    ) -> Result<Option<CheckResult>, AppError> {
        let backend_path = uri
            .path_and_query()
            .ok_or(AppError(String::from("")))?
            .as_str();
        let mut app_config = shared_config.shared_data.lock()?;
        let api_service = app_config
            .api_service_config
            .get_mut(&port)
            .ok_or(AppError(String::from("")))?;

        for item in api_service.service_config.routes.iter_mut() {
            let match_result = item.is_matched(backend_path, Some(headers))?;
            if match_result.clone().is_none() {
                continue;
            }
            let is_allowed = item.is_allowed(&peer_addr, Some(headers))?;
            if !is_allowed {
                return Ok(None);
            }
            let base_route = item.route_cluster.get_route(headers)?;
            let endpoint = base_route.endpoint.clone();
            debug!("The endpoint is {}", endpoint);
            let rest_path = match_result.ok_or("match_result is none")?;

            if endpoint.contains("http") {
                let request_path = [endpoint.as_str(), rest_path.as_str()].join("/");
                spire_context.route = Some(item.clone());
                return Ok(Some(CheckResult {
                    request_path,
                    base_route,
                }));
            } else {
                let path = Path::new(&endpoint);
                let request_path = path.join(rest_path);
                spire_context.route = Some(item.clone());
                return Ok(Some(CheckResult {
                    request_path: String::from(request_path.to_str().unwrap_or_default()),
                    base_route,
                }));
            }
        }
        Ok(None)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::vojo::app_config::ApiService;
    use crate::vojo::app_config::AppConfig;
    use crate::vojo::app_config::Route;
    use crate::vojo::app_config::ServiceConfig;
    use http::HeaderName;
    use http::HeaderValue;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::Mutex;
    #[tokio::test]
    async fn test_check_http_route() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("host"),
            HeaderValue::from_static("test.com"),
        );

        let route = Route::default();

        let mut service_config = ServiceConfig::default();
        service_config.routes.push(route);

        let api_service = ApiService::default();

        let mut config_map = HashMap::new();
        config_map.insert(8080, api_service);

        let shared_config = SharedConfig {
            shared_data: Arc::new(Mutex::new(crate::vojo::app_config::AppConfig {
                api_service_config: config_map,
                static_config: Default::default(),
            })),
        };

        let checker = CommonCheckRequest {};
        let uri = "/api/test/users".parse().unwrap();
        let peer_addr = "127.0.0.1:12345".parse().unwrap();

        let result = checker
            .handle_before_request(
                shared_config,
                8080,
                "test".into(),
                &headers,
                uri,
                peer_addr,
                &mut SpireContext::new(8080, None),
            )
            .await;
        assert!(result.is_err());
        // assert!(result.is_some());
        // let check_result = result.unwrap();
        // assert_eq!(check_result.request_path, "http://backend.test.com/users");
    }

    // 测试文件系统路由匹配
    #[tokio::test]
    async fn test_check_file_route() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("host"),
            HeaderValue::from_static("test.com"),
        );

        let route = Route::default();

        let mut service_config = ServiceConfig::default();
        service_config.routes.push(route);

        let api_service = ApiService::default();

        let mut config_map = HashMap::new();
        config_map.insert(8080, api_service);

        let shared_config = SharedConfig {
            shared_data: Arc::new(Mutex::new(AppConfig {
                api_service_config: config_map,
                static_config: Default::default(),
            })),
        };

        let checker = CommonCheckRequest {};
        let uri = "/static/images/test.jpg".parse().unwrap();
        let peer_addr = "127.0.0.1:12345".parse().unwrap();

        let result = checker
            .handle_before_request(
                shared_config,
                8080,
                "test".into(),
                &headers,
                uri,
                peer_addr,
                &mut SpireContext::new(8080, None),
            )
            .await;
        assert!(result.is_err());
    }

    // 测试不匹配的路由
    #[tokio::test]
    async fn test_check_no_match() {
        let headers = HeaderMap::new();
        let mut config_map = HashMap::new();
        config_map.insert(8080, ApiService::default());

        let shared_config = SharedConfig {
            shared_data: Arc::new(Mutex::new(AppConfig {
                api_service_config: config_map,
                static_config: Default::default(),
            })),
        };

        let checker = CommonCheckRequest {};
        let uri = "/not/exist/path".parse().unwrap();
        let peer_addr = "127.0.0.1:12345".parse().unwrap();

        let result = checker
            .handle_before_request(
                shared_config,
                8080,
                "test".into(),
                &headers,
                uri,
                peer_addr,
                &mut SpireContext::new(8080, None),
            )
            .await
            .unwrap();

        assert!(result.is_none());
    }
}
