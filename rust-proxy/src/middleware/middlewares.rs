use crate::middleware::allow_deny_ip::AllowDenyObject;
use crate::middleware::allow_deny_ip::AllowResult;
use crate::middleware::authentication::Authentication;
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
use std::convert::Infallible;
use std::net::SocketAddr;

use super::forward_header::ForwardHeader;
use super::headers::StaticResourceHeaders;
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "PascalCase")]
pub enum MiddleWares {
    #[serde(rename = "rate_limit")]
    RateLimit(Ratelimit),
    #[serde(rename = "authentication")]
    Authentication(Authentication),
    #[serde(rename = "allow_deny_list")]
    AllowDenyList(Vec<AllowDenyObject>),
    #[serde(rename = "cors")]
    Cors(CorsConfig),
    #[serde(rename = "rewrite_headers")]
    Headers(StaticResourceHeaders),
    #[serde(rename = "forward_headers")]
    ForwardHeader(ForwardHeader),
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
                let is_allowed = ip_is_allowed(Some(allow_deny_list.clone()), peer_addr)?;
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

        response: &mut Response<BoxBody<Bytes, Infallible>>,
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

        req: &mut Request<BoxBody<Bytes, Infallible>>,
    ) -> Result<(), AppError> {
        if let MiddleWares::ForwardHeader(forward_header) = self {
            forward_header.handle_before_request(peer_addr, req)?;
        }
        Ok(())
    }
}
pub fn ip_is_allowed(
    allow_deny_list: Option<Vec<AllowDenyObject>>,
    peer_addr: &SocketAddr,
) -> Result<bool, AppError> {
    if allow_deny_list.is_none()
        || allow_deny_list
            .clone()
            .ok_or("allow_deny_list is none")?
            .is_empty()
    {
        return Ok(true);
    }
    let allow_deny_list = allow_deny_list.ok_or("allow_deny_list is none")?;
    let ip = peer_addr.ip().to_string();
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
