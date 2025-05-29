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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Headers {
    #[serde(rename = "file")]
    StaticSource(StaticResourceHeaders),
}
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

fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("Empty duration string".to_owned());
    }
    let split_pos = s
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| format!("No time unit found in '{}'", s))?;

    let (num_str, unit) = s.split_at(split_pos);
    let num = num_str
        .parse::<u64>()
        .map_err(|e| format!("Invalid number '{}': {}", num_str, e))?;

    let unit = unit.trim().to_lowercase();

    match unit.as_str() {
        "s" => Ok(Duration::from_secs(num)),
        "m" => Ok(Duration::from_secs(num * 60)),
        "h" => Ok(Duration::from_secs(num * 60 * 60)),
        "d" => Ok(Duration::from_secs(num * 60 * 60 * 24)),
        "w" => Ok(Duration::from_secs(num * 60 * 60 * 24 * 7)),
        _ => Err(format!("Unknown time unit '{}'", unit)),
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
impl Headers {
    pub fn handle_before_response(
        &self,
        req_path: &str,

        response: &mut Response<BoxBody<Bytes, Infallible>>,
    ) -> Result<(), AppError> {
        match self {
            Headers::StaticSource(static_source) => {
                static_source.handle_before_response(req_path, response)?;
            }
        }
        Ok(())
    }
}
