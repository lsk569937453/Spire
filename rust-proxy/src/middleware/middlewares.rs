use super::compression::Compression;
use super::forward_header::ForwardHeader;
use super::headers::StaticResourceHeaders;
use crate::middleware::allow_deny_ip::AllowDenyIp;
use crate::middleware::authentication::Authentication;
use crate::middleware::circuit_breaker::CircuitBreaker;
use crate::middleware::cors_config::CorsConfig;
use crate::middleware::rate_limit::Ratelimit;
use crate::middleware::request_headers::RequestHeaders;
use crate::vojo::app_error::AppError;
use async_trait::async_trait;
use bytes::Bytes;
use http::HeaderMap;
use http::HeaderValue;
use http::Request;
use http::Response;
use http::StatusCode;
use http_body_util::combinators::BoxBody;
use serde::Deserialize;
use serde::Serialize;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;

mod arc_mutex_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::sync::{Arc, Mutex};

    pub fn serialize<S, T>(val: &Arc<Mutex<T>>, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        T: Serialize,
    {
        let guard = val.lock().map_err(|e| {
            serde::ser::Error::custom(format!("Mutex poisoned during serialization: {}", e))
        })?;
        T::serialize(&*guard, s)
    }
    pub fn deserialize<'de, D, T>(d: D) -> Result<Arc<Mutex<T>>, D::Error>
    where
        D: Deserializer<'de>,
        T: Deserialize<'de>,
    {
        let data = T::deserialize(d)?;
        Ok(Arc::new(Mutex::new(data)))
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "PascalCase")]
pub enum MiddleWares {
    #[serde(rename = "rate_limit")]
    RateLimit(#[serde(with = "arc_mutex_serde")] Arc<Mutex<Ratelimit>>),
    #[serde(rename = "authentication")]
    Authentication(Authentication),
    #[serde(rename = "allow_deny_list")]
    AllowDenyList(AllowDenyIp),
    #[serde(rename = "cors")]
    Cors(CorsConfig),
    #[serde(rename = "rewrite_headers")]
    Headers(StaticResourceHeaders),
    #[serde(rename = "forward_headers")]
    ForwardHeader(ForwardHeader),
    #[serde(rename = "circuit_breaker")]
    CircuitBreaker(#[serde(with = "arc_mutex_serde")] Arc<Mutex<CircuitBreaker>>),
    #[serde(rename = "request_headers")]
    RequestHeaders(RequestHeaders),
    #[serde(rename = "compression")]
    Compression(Compression),
}
impl PartialEq for MiddleWares {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::RateLimit(a), Self::RateLimit(b)) => Arc::ptr_eq(a, b),

            (Self::Authentication(a), Self::Authentication(b)) => a == b,
            (Self::AllowDenyList(a), Self::AllowDenyList(b)) => a == b,
            (Self::Cors(a), Self::Cors(b)) => a == b,
            (Self::Headers(a), Self::Headers(b)) => a == b,
            (Self::ForwardHeader(a), Self::ForwardHeader(b)) => a == b,
            (Self::CircuitBreaker(a), Self::CircuitBreaker(b)) => Arc::ptr_eq(a, b),

            (Self::RequestHeaders(a), Self::RequestHeaders(b)) => a == b,
            (Self::Compression(a), Self::Compression(b)) => a == b,
            _ => false,
        }
    }
}
#[async_trait]
pub trait Middleware: Send + Sync {
    fn handle_request(
        &mut self,
        _peer_addr: SocketAddr,
        _req: &mut Request<BoxBody<Bytes, AppError>>,
    ) -> Result<(), AppError> {
        Ok(())
    }
    fn check_request(
        &mut self,
        _peer_addr: &SocketAddr,
        _headers: Option<&HeaderMap<HeaderValue>>,
    ) -> Result<CheckResult, AppError> {
        Ok(CheckResult::Allowed)
    }
    async fn handle_response(
        &self,
        _req_path: &str,
        _response: &mut Response<BoxBody<Bytes, AppError>>,
        inbound_headers: HeaderMap,
    ) -> Result<(), AppError> {
        Ok(())
    }
    fn record_outcome(
        &mut self,
        _response_result: &Result<Response<BoxBody<Bytes, AppError>>, AppError>,
    ) {
    }
}
impl Eq for MiddleWares {}
#[derive(Debug, Clone)]
pub struct Denial {
    pub status: StatusCode,
    pub headers: HeaderMap<HeaderValue>,
    pub body: String,
}

#[derive(Debug)]
pub enum CheckResult {
    Allowed,
    Denied(Denial),
}
impl CheckResult {
    pub fn is_allowed(&self) -> bool {
        match self {
            CheckResult::Allowed => true,
            CheckResult::Denied(_) => false,
        }
    }
    pub fn get_denial(&self) -> Option<Denial> {
        match self {
            CheckResult::Allowed => None,
            CheckResult::Denied(denial) => Some(denial.clone()),
        }
    }
}
#[async_trait]
impl Middleware for MiddleWares {
    fn handle_request(
        &mut self,
        peer_addr: SocketAddr,
        req: &mut Request<BoxBody<Bytes, AppError>>,
    ) -> Result<(), AppError> {
        match self {
            MiddleWares::ForwardHeader(mw) => mw.handle_request(peer_addr, req),
            MiddleWares::RequestHeaders(mw) => mw.handle_request(peer_addr, req),

            _ => Ok(()),
        }
    }

    fn check_request(
        &mut self,
        peer_addr: &SocketAddr,
        headers: Option<&HeaderMap<HeaderValue>>,
    ) -> Result<CheckResult, AppError> {
        match self {
            MiddleWares::RateLimit(mw) => mw.check_request(peer_addr, headers),
            MiddleWares::Authentication(mw) => mw.check_request(peer_addr, headers),
            MiddleWares::AllowDenyList(mw) => mw.check_request(peer_addr, headers),
            MiddleWares::CircuitBreaker(mw) => mw.check_request(peer_addr, headers),
            _ => Ok(CheckResult::Allowed),
        }
    }

    async fn handle_response(
        &self,
        req_path: &str,
        response: &mut Response<BoxBody<Bytes, AppError>>,
        inbound_headers: HeaderMap,
    ) -> Result<(), AppError> {
        match self {
            MiddleWares::Cors(mw) => {
                mw.handle_response(req_path, response, inbound_headers)
                    .await
            }
            MiddleWares::Headers(mw) => {
                mw.handle_response(req_path, response, inbound_headers)
                    .await
            }
            MiddleWares::Compression(mw) => {
                mw.handle_response(req_path, response, inbound_headers)
                    .await
            }
            _ => Ok(()),
        }
    }

    fn record_outcome(
        &mut self,
        response_result: &Result<Response<BoxBody<Bytes, AppError>>, AppError>,
    ) {
        if let MiddleWares::CircuitBreaker(mw) = self {
            mw.record_outcome(response_result)
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::middleware::allow_deny_ip::AllowType;
    use crate::middleware::cors_config::{CorsAllowHeader, CorsAllowedOrigins, Method};
    use crate::middleware::{
        allow_deny_ip::AllowDenyItem, authentication::BasicAuth, rate_limit::TokenBucketRateLimit,
    };
    use http::header;
    use std::net::IpAddr;
    use std::net::Ipv4Addr;
    use x509_parser::asn1_rs::Header;
    #[test]
    fn test_rate_limit_middleware() {
        let mut headers = HeaderMap::new();
        headers.insert(header::USER_AGENT, "test-agent".parse().unwrap());
        let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
        let mut middleware = MiddleWares::RateLimit(Arc::new(Mutex::new(Ratelimit::TokenBucket(
            TokenBucketRateLimit::default(),
        ))));

        let result = middleware.check_request(&socket, Some(&headers));
        assert!(result.is_ok());

        let result = middleware.check_request(&socket, Some(&headers));
        assert!(result.is_ok());
    }

    #[test]
    fn test_authentication_middleware() {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, "Bearer test-token".parse().unwrap());
        let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);

        let mut middleware = MiddleWares::Authentication(Authentication::Basic(BasicAuth {
            credentials: "test-token".to_string(),
        }));

        let result = middleware.check_request(&socket, Some(&headers));
        assert!(result.is_ok());

        headers.insert(
            header::AUTHORIZATION,
            "Bearer invalid-token".parse().unwrap(),
        );
        let result = middleware.check_request(&socket, Some(&headers));
        assert!(result.is_ok());
    }

    #[test]
    fn test_allow_deny_list_middleware() {
        let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
        let mut middleware = MiddleWares::AllowDenyList(AllowDenyIp {
            rules: vec![AllowDenyItem {
                policy: AllowType::Allow,
                value: Some("127.0.0.1".to_string()),
            }],
        });

        let result = middleware.check_request(&socket, None);
        assert!(result.is_ok());

        let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), 8080);
        let result = middleware.check_request(&socket, None);
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_cors_middleware() {
        let cors_config = CorsConfig {
            allowed_origins: CorsAllowedOrigins::All,
            allowed_methods: vec![Method::Get],
            allowed_headers: Some(CorsAllowHeader::All),
            allow_credentials: Some(true),
            max_age: None,
            options_passthrough: None,
        };
        let middleware = MiddleWares::Cors(cors_config);

        let mut response = Response::builder().body(BoxBody::default()).unwrap();

        let result = middleware
            .handle_response("", &mut response, HeaderMap::new())
            .await;
        assert!(result.is_ok());

        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .unwrap(),
            "*"
        );
    }

    #[test]
    fn test_forward_header_middleware() {
        let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
        let mut middleware = MiddleWares::ForwardHeader(ForwardHeader {});

        let mut request = Request::builder().body(BoxBody::default()).unwrap();

        let result = middleware.handle_request(socket, &mut request);
        assert!(result.is_ok());

        assert_eq!(
            request.headers().get("X-Forwarded-For").unwrap(),
            "127.0.0.1"
        );
    }
}
