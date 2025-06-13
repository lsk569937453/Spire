use super::forward_header::ForwardHeader;
use super::headers::StaticResourceHeaders;
use crate::middleware::allow_deny_ip::AllowDenyIp;
use crate::middleware::authentication::Authentication;
use crate::middleware::circuit_breaker::CircuitBreaker;
use crate::middleware::cors_config::CorsConfig;
use crate::middleware::rate_limit::Ratelimit;
use crate::AppError;
use bytes::Bytes;
use http::HeaderMap;
use http::HeaderValue;
use http::Request;
use http::Response;
use http_body_util::combinators::BoxBody;
use serde::Deserialize;
use serde::Serialize;
use std::net::SocketAddr;
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "PascalCase")]
pub enum MiddleWares {
    #[serde(rename = "rate_limit")]
    RateLimit(Ratelimit),
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

    CircuitBreaker(CircuitBreaker),
}
impl MiddleWares {
    pub fn is_allowed(
        &mut self,
        peer_addr: &SocketAddr,
        headers_option: Option<&HeaderMap<HeaderValue>>,
    ) -> Result<bool, AppError> {
        match self {
            MiddleWares::RateLimit(ratelimit) => {
                if let Some(header_map) = headers_option {
                    let is_allowed = !ratelimit.should_limit(header_map, peer_addr)?;
                    if !is_allowed {
                        return Ok(is_allowed);
                    }
                }
            }
            MiddleWares::Authentication(authentication) => {
                if let Some(header_map) = headers_option {
                    let is_allowed = authentication.check_authentication(header_map)?;
                    if !is_allowed {
                        return Ok(is_allowed);
                    }
                }
            }
            MiddleWares::AllowDenyList(allow_deny_list) => {
                let is_allowed = allow_deny_list.ip_is_allowed(peer_addr)?;
                if !is_allowed {
                    return Ok(is_allowed);
                }
            }
            _ => {}
        }
        Ok(true)
    }
    pub fn handle_before_response(
        &self,
        req_path: &str,

        response: &mut Response<BoxBody<Bytes, AppError>>,
    ) -> Result<(), AppError> {
        match self {
            MiddleWares::Cors(cors_config) => {
                cors_config.handle_before_response(response)?;
            }
            MiddleWares::Headers(headers) => {
                headers.handle_before_response(req_path, response)?;
            }
            _ => {}
        }
        Ok(())
    }
    pub fn handle_before_request(
        &self,
        peer_addr: SocketAddr,

        req: &mut Request<BoxBody<Bytes, AppError>>,
    ) -> Result<(), AppError> {
        if let MiddleWares::ForwardHeader(forward_header) = self {
            forward_header.handle_before_request(peer_addr, req)?;
        }
        Ok(())
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
    #[test]
    fn test_rate_limit_middleware() {
        let mut headers = HeaderMap::new();
        headers.insert(header::USER_AGENT, "test-agent".parse().unwrap());
        let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
        println!("a-----------------");
        let mut middleware =
            MiddleWares::RateLimit(Ratelimit::TokenBucket(TokenBucketRateLimit::default()));

        let result = middleware.is_allowed(&socket, Some(&headers));
        assert!(result.is_ok());
        assert!(result.unwrap());

        let result = middleware.is_allowed(&socket, Some(&headers));
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[test]
    fn test_authentication_middleware() {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, "Bearer test-token".parse().unwrap());
        let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);

        let mut middleware = MiddleWares::Authentication(Authentication::Basic(BasicAuth {
            credentials: "test-token".to_string(),
        }));

        let result = middleware.is_allowed(&socket, Some(&headers));
        assert!(result.is_ok());
        assert!(!result.unwrap());

        headers.insert(
            header::AUTHORIZATION,
            "Bearer invalid-token".parse().unwrap(),
        );
        let result = middleware.is_allowed(&socket, Some(&headers));
        assert!(result.is_ok());
        assert!(!result.unwrap());
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

        let result = middleware.is_allowed(&socket, None);
        assert!(result.is_ok());
        assert!(result.unwrap());

        let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), 8080);
        let result = middleware.is_allowed(&socket, None);
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[test]
    fn test_cors_middleware() {
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

        let result = middleware.handle_before_response("", &mut response);
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
        let middleware = MiddleWares::ForwardHeader(ForwardHeader {});

        let mut request = Request::builder().body(BoxBody::default()).unwrap();

        let result = middleware.handle_before_request(socket, &mut request);
        assert!(result.is_ok());

        assert_eq!(
            request.headers().get("X-Forwarded-For").unwrap(),
            "127.0.0.1"
        );
    }
}
