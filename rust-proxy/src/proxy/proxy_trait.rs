use crate::vojo::app_config::Route;
use crate::vojo::app_error::AppError;
use crate::vojo::route::BaseRoute;
use crate::SharedConfig;
use http::HeaderMap;
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
impl CommonCheckRequest {
    pub fn new() -> Self {
        CommonCheckRequest {}
    }
}
#[derive(Debug, Clone)]
pub struct CheckResult {
    pub request_path: String,
    pub route: Route,
    pub base_route: BaseRoute,
}

impl CheckTrait for CommonCheckRequest {
    async fn check_before_request(
        &self,
        shared_config: SharedConfig,
        port: i32,
        mapping_key: String,
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
            .map_err(|e| AppError(e.to_string()))?
          ;
        let  api_service = app_config
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
            let is_allowed = item
                .is_allowed(addr_string1, Some(headers1))
                ?;
            if !is_allowed {
                return Ok(None);
            }
            let base_route = item
                .route_cluster
                .get_route(headers.clone())
                ?;
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
                    route: item.clone(),
                    base_route,
                }));
            } else {
                let path = Path::new(&endpoint);
                let rest_path = match_result.unwrap();
                let request_path = path.join(rest_path);
                return Ok(Some(CheckResult {
                    request_path: String::from(request_path.to_str().unwrap_or_default()),
                    route: item.clone(),
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
    #[test]
    fn test_url_parse() {
        let host = Url::parse("http://127.0.0.1:8080");
        assert!(host.is_ok());
    }
}
