use crate::AppError;
use bytes::Bytes;
use http::header;
use http::Response;
use http_body_util::combinators::BoxBody;
use serde::de;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use std::convert::Infallible;
use std::time::Duration;
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StaticResourceHeaders {
    expires: Duration,
    extensions: Vec<String>,
}
impl<'de> Deserialize<'de> for StaticResourceHeaders {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Inner {
            expires: String,
            extensions: Vec<String>,
        }

        let inner = Inner::deserialize(deserializer)?;

        let expires = parse_duration(&inner.expires).map_err(|e| {
            de::Error::custom(format!(
                "Invalid duration format '{}': {}. \
            Expected format like: 30d, 1h, 90m (supported units: s, m, h, d, w)",
                inner.expires, e
            ))
        })?;

        Ok(StaticResourceHeaders {
            expires,
            extensions: inner.extensions,
        })
    }
}

fn parse_duration(s: &str) -> Result<Duration, AppError> {
    let s = s.trim();
    if s.is_empty() {
        Err("Empty duration string")?;
    }
    let split_pos = s
        .find(|c: char| !c.is_ascii_digit())
        .ok_or("is_ascii_digit cause error")?;

    let (num_str, unit) = s.split_at(split_pos);
    let num = num_str.parse::<u64>()?;

    let unit = unit.trim().to_lowercase();

    match unit.as_str() {
        "s" => Ok(Duration::from_secs(num)),
        "m" => Ok(Duration::from_secs(num * 60)),
        "h" => Ok(Duration::from_secs(num * 60 * 60)),
        "d" => Ok(Duration::from_secs(num * 60 * 60 * 24)),
        "w" => Ok(Duration::from_secs(num * 60 * 60 * 24 * 7)),
        _ => Err(AppError(format!("Unknown time unit '{}'", unit))),
    }
}
impl StaticResourceHeaders {
    pub fn handle_before_response(
        &self,
        req_path: &str,

        response: &mut Response<BoxBody<Bytes, Infallible>>,
    ) -> Result<(), AppError> {
        for item in self.extensions.iter() {
            if req_path.ends_with(item) {
                let cache_control = format!("public, max-age={}", self.expires.as_secs());
                response
                    .headers_mut()
                    .insert(header::CACHE_CONTROL, cache_control.parse().unwrap());

                let expires = SystemTime::now() + self.expires;
                let expires_str = httpdate::fmt_http_date(expires);
                response
                    .headers_mut()
                    .insert(header::EXPIRES, expires_str.parse().unwrap());
            }
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use http_body_util::BodyExt;
    use http_body_util::Full;

    use super::*;
    #[test]
    fn test_parse_duration() {
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(parse_duration("1d").unwrap(), Duration::from_secs(86400));
        assert_eq!(parse_duration("1w").unwrap(), Duration::from_secs(604800));

        // 测试错误情况
        assert!(parse_duration("").is_err());
        assert!(parse_duration("30x").is_err());
        assert!(parse_duration("abc").is_err());
    }

    #[test]
    fn test_static_resource_headers_deserialize() {
        let json = r#"{
            "expires": "1d",
            "extensions": [".jpg", ".png"]
        }"#;

        let headers: StaticResourceHeaders = serde_json::from_str(json).unwrap();
        assert_eq!(headers.expires, Duration::from_secs(86400));
        assert_eq!(
            headers.extensions,
            vec![".jpg".to_string(), ".png".to_string()]
        );
    }

    #[test]
    fn test_handle_before_response() {
        let headers = StaticResourceHeaders {
            expires: Duration::from_secs(3600),
            extensions: vec![".jpg".to_string()],
        };

        let mut response = Response::new(Full::new(Bytes::from("test")).boxed());
        headers
            .handle_before_response("test.jpg", &mut response)
            .unwrap();

        assert!(response.headers().contains_key(header::CACHE_CONTROL));
        assert!(response.headers().contains_key(header::EXPIRES));

        let mut response = Response::new(Full::new(Bytes::from("test")).boxed());
        headers
            .handle_before_response("test.txt", &mut response)
            .unwrap();

        assert!(!response.headers().contains_key(header::CACHE_CONTROL));
        assert!(!response.headers().contains_key(header::EXPIRES));
    }
}
