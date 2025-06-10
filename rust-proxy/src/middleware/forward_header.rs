use crate::AppError;
use bytes::Bytes;
use http::Request;
use http_body_util::combinators::BoxBody;
use serde::Deserialize;
use serde::Serialize;
use std::net::SocketAddr;
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForwardHeader {}
impl ForwardHeader {
    pub fn handle_before_request(
        &self,
        peer_addr: SocketAddr,

        req: &mut Request<BoxBody<Bytes, AppError>>,
    ) -> Result<(), AppError> {
        let client_ip = peer_addr.ip().to_string();
        req.headers_mut().insert("X-Real-IP", client_ip.parse()?);

        if let Some(existing_forwarded) = req.headers().get("X-Forwarded-For") {
            let new_value = format!("{}, {}", existing_forwarded.to_str()?, client_ip);
            req.headers_mut()
                .insert("X-Forwarded-For", new_value.parse()?);
        } else {
            req.headers_mut()
                .insert("X-Forwarded-For", client_ip.parse()?);
        }

        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;
    use http_body_util::BodyExt;
    use http_body_util::Full;
    #[test]
    fn test_handle_before_request() {
        let forward_header = ForwardHeader {};

        let peer_addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();

        let mut req = Request::builder()
            .uri("http://example.com")
            .body(Full::new(Bytes::new()).map_err(AppError::from).boxed())
            .unwrap();

        let result = forward_header.handle_before_request(peer_addr, &mut req);
        assert!(result.is_ok());

        assert_eq!(
            req.headers().get("X-Real-IP").unwrap(),
            &HeaderValue::from_static("127.0.0.1")
        );

        assert_eq!(
            req.headers().get("X-Forwarded-For").unwrap(),
            &HeaderValue::from_static("127.0.0.1")
        );
    }

    #[test]
    fn test_handle_before_request_with_existing_forwarded() {
        let forward_header = ForwardHeader {};
        let peer_addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();

        let mut req = Request::builder()
            .uri("http://example.com")
            .header("X-Forwarded-For", "192.168.1.1")
            .body(Full::new(Bytes::new()).map_err(AppError::from).boxed())
            .unwrap();

        let result = forward_header.handle_before_request(peer_addr, &mut req);
        assert!(result.is_ok());

        assert_eq!(
            req.headers().get("X-Forwarded-For").unwrap(),
            &HeaderValue::from_static("192.168.1.1, 127.0.0.1")
        );
    }
}
