use crate::middleware::cors_config::CorsConfig;
use crate::middleware::middlewares::MiddleWares;
use crate::vojo::app_error::AppError;
use crate::vojo::router::BaseRoute;
use crate::vojo::router::StaticFileRoute;
use crate::SharedConfig;
use bytes::Bytes;
use http::header;
use http::header::HeaderMap;
use http::HeaderValue;
use http::Request;
use http::StatusCode;
use http_body_util::combinators::BoxBody;
use http_body_util::BodyExt;
use http_body_util::Full;
use hyper::Response;
use hyper::Uri;
use mockall::automock;
use serde::Deserialize;
use serde::Serialize;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::Path;
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpireContext {
    pub port: i32,
    pub middlewares: Option<Vec<MiddleWares>>,
}
impl SpireContext {
    pub fn new(port: i32, middlewares: Option<Vec<MiddleWares>>) -> Self {
        Self { port, middlewares }
    }
    pub fn cors_configed(&self) -> Result<Option<CorsConfig>, AppError> {
        if let Some(middlewares) = &self.middlewares {
            for middleware in middlewares.iter() {
                if let MiddleWares::Cors(s) = middleware {
                    return Ok(Some(s.clone()));
                }
            }
        }
        Ok(None)
    }
}
#[automock]
pub trait ChainTrait {
    async fn get_destination(
        &self,
        shared_config: SharedConfig,
        port: i32,
        mapping_key: String,
        headers: &HeaderMap,
        uri: Uri,
        peer_addr: SocketAddr,
        spire_context: &mut SpireContext,
    ) -> Result<Option<HandlingResult>, AppError>;
    async fn handle_before_response(
        &self,
        middlewares: Vec<MiddleWares>,
        req_path: &str,
        response: &mut Response<BoxBody<Bytes, Infallible>>,
    ) -> Result<(), AppError>;
    fn handle_preflight(
        &self,
        cors_config: CorsConfig,
        origin: &str,
    ) -> Result<Response<BoxBody<Bytes, Infallible>>, AppError>;
    async fn handle_before_request(
        &self,
        middlewares: Vec<MiddleWares>,
        peer_addr: SocketAddr,
        req: &mut Request<BoxBody<Bytes, Infallible>>,
    ) -> Result<(), AppError>;
}
pub struct CommonCheckRequest;

#[derive(Debug, Clone)]
pub struct HandlingResult {
    pub request_path: String,
    pub router_destination: RouterDestination,
}
#[derive(Debug, Clone, PartialEq, Eq)]

pub enum RouterDestination {
    Http(BaseRoute),
    File(StaticFileRoute),
}
impl RouterDestination {
    pub fn get_endpoint(&self) -> String {
        match self {
            RouterDestination::Http(base_route) => base_route.endpoint.clone(),
            RouterDestination::File(static_file_route) => static_file_route.doc_root.clone(),
        }
    }
    pub fn is_http(&self) -> bool {
        match self {
            RouterDestination::Http(_) => true,
            RouterDestination::File(_) => false,
        }
    }
    pub fn is_file(&self) -> bool {
        match self {
            RouterDestination::Http(_) => false,
            RouterDestination::File(_) => true,
        }
    }
}
impl ChainTrait for CommonCheckRequest {
    async fn handle_before_response(
        &self,
        middlewares: Vec<MiddleWares>,
        req_path: &str,

        response: &mut Response<BoxBody<Bytes, Infallible>>,
    ) -> Result<(), AppError> {
        for item in middlewares.iter() {
            item.handle_before_response(req_path, response)?;
        }

        Ok(())
    }
    async fn handle_before_request(
        &self,
        middlewares: Vec<MiddleWares>,

        peer_addr: SocketAddr,
        req: &mut Request<BoxBody<Bytes, Infallible>>,
    ) -> Result<(), AppError> {
        for item in middlewares.iter() {
            item.handle_before_request(peer_addr, req)?;
        }
        Ok(())
    }
    async fn get_destination(
        &self,
        shared_config: SharedConfig,
        port: i32,
        _mapping_key: String,
        headers: &HeaderMap,
        uri: Uri,
        peer_addr: SocketAddr,
        spire_context: &mut SpireContext,
    ) -> Result<Option<HandlingResult>, AppError> {
        let backend_path = uri
            .path_and_query()
            .ok_or(AppError::from("Path is empty"))?
            .as_str();
        let mut app_config = shared_config.shared_data.lock()?;
        let api_service = app_config
            .api_service_config
            .get_mut(&port)
            .ok_or(AppError::from(
                "Can not find config by port from app config.",
            ))?;

        for item in api_service.service_config.route_configs.iter_mut() {
            let match_result = item.is_matched(backend_path, Some(headers))?;
            if match_result.is_none() {
                continue;
            }
            let is_allowed = item.is_allowed(&peer_addr, Some(headers))?;
            if !is_allowed {
                return Ok(None);
            }
            let router_destination = item.router.get_route(headers)?;
            let rest_path = match_result.ok_or("match_result is none")?;

            match router_destination {
                RouterDestination::File(file_route) => {
                    let path = Path::new(&file_route.doc_root);
                    let request_path = path.join(rest_path);
                    spire_context.middlewares = item.middlewares.clone();
                    return Ok(Some(HandlingResult {
                        request_path: String::from(request_path.to_str().unwrap_or_default()),
                        router_destination: RouterDestination::File(file_route),
                    }));
                }
                RouterDestination::Http(base_route) => {
                    let request_path = [base_route.endpoint.as_str(), rest_path.as_str()].join("/");
                    spire_context.middlewares = item.middlewares.clone();
                    return Ok(Some(HandlingResult {
                        request_path,
                        router_destination: RouterDestination::Http(base_route.clone()),
                    }));
                }
            }
        }
        Ok(None)
    }
    fn handle_preflight(
        &self,
        cors_config: CorsConfig,
        origin: &str,
    ) -> Result<Response<BoxBody<Bytes, Infallible>>, AppError> {
        if cors_config.validate_origin("")? {
            return Ok(Response::builder()
                .status(StatusCode::FORBIDDEN)
                .body(Full::new(Bytes::from("".to_string())).boxed())?);
        }
        let methods_header = cors_config
            .allowed_methods
            .iter()
            .map(|m| m.as_str()) // Convert serde_json::Error to AppError
            .collect::<Vec<&str>>()
            .join(", ");
        let headers_header = cors_config
            .allowed_headers
            .iter()
            .map(|m| m.to_string()) // Convert serde_json::Error to AppError
            .collect::<Vec<String>>()
            .join(", ");

        let mut builder = Response::builder()
            .status(StatusCode::NO_CONTENT)
            .header(header::ACCESS_CONTROL_ALLOW_METHODS, methods_header)
            .header(header::ACCESS_CONTROL_ALLOW_HEADERS, headers_header)
            .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin)
            .header(header::VARY, "Origin");

        if let Some(s) = cors_config.allow_credentials {
            if s {
                builder = builder.header(
                    header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
                    HeaderValue::from_static("true"),
                );
            }
        }
        if let Some(s) = cors_config.max_age {
            builder = builder.header(header::ACCESS_CONTROL_MAX_AGE, s.to_string());
        }

        Ok(builder.body(Full::new(Bytes::from("".to_string())).boxed())?)
    }
}
#[automock]
pub trait ChainTrait {
    #[allow(clippy::too_many_arguments)]
    async fn get_destination(
        &self,
        shared_config: SharedConfig,
        port: i32,
        mapping_key: String,
        headers: &HeaderMap,
        uri: Uri,
        peer_addr: SocketAddr,
        spire_context: &mut SpireContext,
    ) -> Result<Option<HandlingResult>, AppError>;
    async fn handle_before_response(
        &self,
        middlewares: Vec<MiddleWares>,
        req_path: &str,
        response: &mut Response<BoxBody<Bytes, AppError>>,
    ) -> Result<(), AppError>;
    fn handle_preflight(
        &self,
        cors_config: CorsConfig,
        origin: &str,
    ) -> Result<Response<BoxBody<Bytes, AppError>>, AppError>;
    async fn handle_before_request(
        &self,
        middlewares: Vec<MiddleWares>,
        peer_addr: SocketAddr,
        req: &mut Request<BoxBody<Bytes, AppError>>,
    ) -> Result<(), AppError>;
}
pub struct CommonCheckRequest;

#[derive(Debug, Clone)]
pub struct HandlingResult {
    pub request_path: String,
    pub router_destination: RouterDestination,
}
#[derive(Debug, Clone, PartialEq, Eq)]

pub enum RouterDestination {
    Http(BaseRoute),
    File(StaticFileRoute),
}
impl RouterDestination {
    pub fn get_endpoint(&self) -> String {
        match self {
            RouterDestination::Http(base_route) => base_route.endpoint.clone(),
            RouterDestination::File(static_file_route) => static_file_route.doc_root.clone(),
        }
    }

    pub fn is_file(&self) -> bool {
        match self {
            RouterDestination::Http(_) => false,
            RouterDestination::File(_) => true,
        }
    }
}
impl ChainTrait for CommonCheckRequest {
    async fn handle_before_response(
        &self,
        middlewares: Vec<MiddleWares>,
        req_path: &str,

        response: &mut Response<BoxBody<Bytes, AppError>>,
    ) -> Result<(), AppError> {
        for item in middlewares.iter() {
            item.handle_before_response(req_path, response)?;
        }

        Ok(())
    }
    async fn handle_before_request(
        &self,
        middlewares: Vec<MiddleWares>,

        peer_addr: SocketAddr,
        req: &mut Request<BoxBody<Bytes, AppError>>,
    ) -> Result<(), AppError> {
        for item in middlewares.iter() {
            item.handle_before_request(peer_addr, req)?;
        }
        Ok(())
    }
    async fn get_destination(
        &self,
        shared_config: SharedConfig,
        port: i32,
        _mapping_key: String,
        headers: &HeaderMap,
        uri: Uri,
        peer_addr: SocketAddr,
        spire_context: &mut SpireContext,
    ) -> Result<Option<HandlingResult>, AppError> {
        let backend_path = uri
            .path_and_query()
            .ok_or(AppError::from("Path is empty"))?
            .as_str();
        let mut app_config = shared_config.shared_data.lock()?;
        let api_service = app_config
            .api_service_config
            .get_mut(&port)
            .ok_or(AppError::from(
                "Can not find config by port from app config.",
            ))?;

        for item in api_service.route_configs.iter_mut() {
            let match_result = item.is_matched(backend_path, Some(headers))?;
            if match_result.is_none() {
                continue;
            }
            let is_allowed = item.is_allowed(&peer_addr, Some(headers))?;
            if !is_allowed {
                return Ok(None);
            }
            let router_destination = item.router.get_route(headers)?;
            let rest_path = match_result.ok_or("match_result is none")?;

            match router_destination {
                RouterDestination::File(file_route) => {
                    let path = Path::new(&file_route.doc_root);
                    let request_path = path.join(rest_path);
                    spire_context.middlewares = item.middlewares.clone();
                    return Ok(Some(HandlingResult {
                        request_path: String::from(request_path.to_str().unwrap_or_default()),
                        router_destination: RouterDestination::File(file_route),
                    }));
                }
                RouterDestination::Http(base_route) => {
                    let request_path = [base_route.endpoint.as_str(), rest_path.as_str()].join("/");
                    spire_context.middlewares = item.middlewares.clone();
                    return Ok(Some(HandlingResult {
                        request_path,
                        router_destination: RouterDestination::Http(base_route.clone()),
                    }));
                }
            }
        }
        Ok(None)
    }
    fn handle_preflight(
        &self,
        cors_config: CorsConfig,
        origin: &str,
    ) -> Result<Response<BoxBody<Bytes, AppError>>, AppError> {
        if cors_config.validate_origin("")? {
            return Ok(Response::builder().status(StatusCode::FORBIDDEN).body(
                Full::new(Bytes::from("".to_string()))
                    .map_err(AppError::from)
                    .boxed(),
            )?);
        }
        let methods_header = cors_config
            .allowed_methods
            .iter()
            .map(|m| m.as_str()) // Convert serde_json::Error to AppError
            .collect::<Vec<&str>>()
            .join(", ");
        let headers_header = cors_config
            .allowed_headers
            .iter()
            .map(|m| m.to_string()) // Convert serde_json::Error to AppError
            .collect::<Vec<String>>()
            .join(", ");

        let mut builder = Response::builder()
            .status(StatusCode::NO_CONTENT)
            .header(header::ACCESS_CONTROL_ALLOW_METHODS, methods_header)
            .header(header::ACCESS_CONTROL_ALLOW_HEADERS, headers_header)
            .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin)
            .header(header::VARY, "Origin");

        if let Some(s) = cors_config.allow_credentials {
            if s {
                builder = builder.header(
                    header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
                    HeaderValue::from_static("true"),
                );
            }
        }
        if let Some(s) = cors_config.max_age {
            builder = builder.header(header::ACCESS_CONTROL_MAX_AGE, s.to_string());
        }

        Ok(builder.body(
            Full::new(Bytes::from("".to_string()))
                .map_err(AppError::from)
                .boxed(),
        )?)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::vojo::app_config::ApiService;
    use crate::vojo::app_config::AppConfig;
    use crate::vojo::app_config::RouteConfig;
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

        let route = RouteConfig::default();

        let mut service_config = ServiceConfig::default();
        service_config.route_configs.push(route);

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
            .get_destination(
                shared_config,
                8080,
                "test".into(),
                &headers,
                uri,
                peer_addr,
                &mut SpireContext::new(8080, None),
            )
            .await;
        assert!(result.is_ok());
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

        let route = RouteConfig::default();

        let mut service_config = ServiceConfig::default();
        service_config.route_configs.push(route);

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
            .get_destination(
                shared_config,
                8080,
                "test".into(),
                &headers,
                uri,
                peer_addr,
                &mut SpireContext::new(8080, None),
            )
            .await;
        assert!(result.is_ok());
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
            .get_destination(
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
