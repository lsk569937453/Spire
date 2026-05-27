use crate::middleware::middlewares::Middleware;
use crate::vojo::app_error::AppError;
use http::HeaderMap;
use serde::Deserialize;
use serde::Serialize;
use std::net::SocketAddr;

// 只负责添加 X-Real-IP 和 X-Forwarded-For。它不需要任何配置字段。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForwardHeader {}
impl Middleware for ForwardHeader {
    fn handle_request(
        &mut self,
        peer_addr: SocketAddr,
        headers: &mut HeaderMap,
    ) -> Result<(), AppError> {
        self.handle_before_request(peer_addr, headers)
    }
}
impl ForwardHeader {
    pub fn handle_before_request(
        &self,
        peer_addr: SocketAddr,
        headers: &mut HeaderMap,
    ) -> Result<(), AppError> {
        let client_ip = peer_addr.ip().to_string();
        headers.insert("X-Real-IP", client_ip.parse()?);

        if let Some(existing_forwarded) = headers.get("X-Forwarded-For") {
            let new_value = format!("{}, {}", existing_forwarded.to_str()?, client_ip);
            headers.insert("X-Forwarded-For", new_value.parse()?);
        } else {
            headers.insert("X-Forwarded-For", client_ip.parse()?);
        }

        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;
    #[test]
    fn test_handle_before_request() {
        let forward_header = ForwardHeader {};

        let peer_addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();

        let mut headers = HeaderMap::new();

        let result = forward_header.handle_before_request(peer_addr, &mut headers);
        assert!(result.is_ok());

        assert_eq!(
            headers.get("X-Real-IP").unwrap(),
            &HeaderValue::from_static("127.0.0.1")
        );

        assert_eq!(
            headers.get("X-Forwarded-For").unwrap(),
            &HeaderValue::from_static("127.0.0.1")
        );
    }

    #[test]
    fn test_handle_before_request_with_existing_forwarded() {
        let forward_header = ForwardHeader {};
        let peer_addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();

        let mut headers = HeaderMap::new();
        headers.insert("X-Forwarded-For", "192.168.1.1".parse().unwrap());

        let result = forward_header.handle_before_request(peer_addr, &mut headers);
        assert!(result.is_ok());

        assert_eq!(
            headers.get("X-Forwarded-For").unwrap(),
            &HeaderValue::from_static("192.168.1.1, 127.0.0.1")
        );
    }
}
