use crate::middleware::middlewares::Middleware;
use crate::vojo::app_error::AppError;
use http::header::{HeaderName, HeaderValue};
use http::HeaderMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::str::FromStr;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RequestHeaders {
    #[serde(default)]
    pub add: HashMap<String, String>,
    #[serde(default)]
    pub remove: Vec<String>,
}

impl Middleware for RequestHeaders {
    fn handle_request(
        &mut self,
        _peer_addr: SocketAddr,
        headers: &mut HeaderMap,
    ) -> Result<(), AppError> {
        for key in &self.remove {
            if let Ok(header_name) = HeaderName::from_str(key) {
                headers.remove(header_name);
            }
        }
        for (key, value) in &self.add {
            let header_name = HeaderName::from_str(key)
                .map_err(|e| AppError(format!("Invalid header name '{key}': {e}")))?;

            let header_value = HeaderValue::from_str(value)
                .map_err(|e| AppError(format!("Invalid header value for '{key}': {e}")))?;
            debug!("Adding header: {key}: {value}");
            headers.insert(header_name, header_value);
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_and_remove_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("X-Existing-Header", HeaderValue::from_static("present"));
        headers.insert("X-Header-To-Remove", HeaderValue::from_static("remove-me"));

        let mut middleware = RequestHeaders {
            add: {
                let mut add_headers = HashMap::new();
                add_headers.insert("X-New-Header".to_string(), "new-value".to_string());
                add_headers
            },
            remove: vec!["X-Header-To-Remove".to_string()],
        };

        let peer_addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();

        let result = middleware.handle_request(peer_addr, &mut headers);

        assert!(result.is_ok());

        assert_eq!(headers.get("X-New-Header").unwrap(), "new-value");
        assert!(!headers.contains_key("X-Header-To-Remove"));
        assert!(headers.contains_key("X-Existing-Header"));
    }

    #[test]
    fn test_remove_nonexistent_header() {
        let mut headers = HeaderMap::new();

        let mut middleware = RequestHeaders {
            add: HashMap::new(),
            remove: vec!["X-Nonexistent-Header".to_string()],
        };

        let peer_addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();

        let result = middleware.handle_request(peer_addr, &mut headers);

        assert!(result.is_ok());

        assert!(headers.is_empty());
    }

    #[test]
    fn test_invalid_header_name() {
        let mut headers = HeaderMap::new();

        let mut middleware = RequestHeaders {
            add: {
                let mut add_headers = HashMap::new();
                add_headers.insert("Invalid Header Name".to_string(), "value".to_string());
                add_headers
            },
            remove: Vec::new(),
        };

        let peer_addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();

        let result = middleware.handle_request(peer_addr, &mut headers);

        assert!(result.is_err());
        if let Err(e) = result {
            assert!(e.0.contains("Invalid header name"));
        }
    }

    #[test]
    fn test_invalid_header_value() {
        let mut headers = HeaderMap::new();

        let mut middleware = RequestHeaders {
            add: {
                let mut add_headers = HashMap::new();
                add_headers.insert("X-Valid-Header".to_string(), "Invalid\r\nValue".to_string());
                add_headers
            },
            remove: Vec::new(),
        };

        let peer_addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();

        let result = middleware.handle_request(peer_addr, &mut headers);

        assert!(result.is_err());
        if let Err(e) = result {
            assert!(e.0.contains("Invalid header value"));
        }
    }
}
