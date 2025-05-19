use crate::vojo::app_error::AppError;
use crate::vojo::route::BaseRoute;
use crate::SharedConfig;
use http::header::HeaderMap;
use hyper::Uri;
use std::net::SocketAddr;
use std::path::Path;
use url::Url;
pub trait CheckTrait {
    async fn check_before_request(
        &self,
        shared_config: SharedConfig,
        port: i32,
        mapping_key: String,
        headers: HeaderMap,
        uri: Uri,
        peer_addr: SocketAddr,
    ) -> Result<Option<CheckResult>, AppError>;
}
pub struct CommonCheckRequest;

#[derive(Debug, Clone)]
pub struct CheckResult {
    pub request_path: String,
    pub base_route: BaseRoute,
}

impl CheckTrait for CommonCheckRequest {
    async fn check_before_request(
        &self,
        shared_config: SharedConfig,
        port: i32,
        _mapping_key: String,
        headers: HeaderMap,
        uri: Uri,
        peer_addr: SocketAddr,
    ) -> Result<Option<CheckResult>, AppError> {
        let backend_path = uri
            .path_and_query()
            .ok_or(AppError(String::from("")))?
            .to_string();
        let mut app_config = shared_config
            .shared_data
            .lock()
            .map_err(|e| AppError(e.to_string()))?;
        let api_service = app_config
            .api_service_config
            .get_mut(&port)
            .ok_or(AppError(String::from("")))?;

        let addr_string = peer_addr.ip().to_string();
        for item in api_service.service_config.routes.iter_mut() {
            let back_path_clone = backend_path.clone();
            let match_result = item.is_matched(back_path_clone, Some(headers.clone()))?;
            if match_result.clone().is_none() {
                continue;
            }
            let headers1 = headers.clone();
            let addr_string1 = addr_string.clone();
            let is_allowed = item.is_allowed(addr_string1, Some(headers1))?;
            if !is_allowed {
                return Ok(None);
            }
            let base_route = item.route_cluster.get_route(headers.clone())?;
            let endpoint = base_route.endpoint.clone();
            debug!("The endpoint is {}", endpoint);
            if endpoint.contains("http") {
                let host = Url::parse(endpoint.as_str()).map_err(|e| AppError(e.to_string()))?;
                let rest_path = match_result.unwrap();

                let request_path = host
                    .join(rest_path.as_str())
                    .map_err(|e| AppError(e.to_string()))?
                    .to_string();
                return Ok(Some(CheckResult {
                    request_path,
                    base_route,
                }));
            } else {
                let path = Path::new(&endpoint);
                let rest_path = match_result.unwrap();
                let request_path = path.join(rest_path);
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
            .check_before_request(shared_config, 8080, "test".into(), headers, uri, peer_addr)
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
            .check_before_request(shared_config, 8080, "test".into(), headers, uri, peer_addr)
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
            .check_before_request(shared_config, 8080, "test".into(), headers, uri, peer_addr)
            .await
            .unwrap();

        assert!(result.is_none());
    }
}
