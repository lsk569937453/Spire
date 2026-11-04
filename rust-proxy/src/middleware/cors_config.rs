use crate::middleware::middlewares::Middleware;
use crate::vojo::app_error::AppError;
use async_trait::async_trait;
use bytes::Bytes;
use http::HeaderMap;
use http::HeaderValue;
use http::Response;
use http::StatusCode;
use http::header;
use http_body_util::BodyExt;
use http_body_util::Full;
use http_body_util::combinators::BoxBody;
use regex::Regex;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::Serializer;
use serde::de;
use serde::de::SeqAccess;
use serde::de::Visitor;
use serde::de::value::SeqAccessDeserializer;
use serde::ser::SerializeSeq;
use std::fmt;
use std::fmt::Display;
use std::sync::OnceLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorsConfig {
    pub allowed_origins: CorsAllowedOrigins,
    pub allowed_methods: Vec<Method>,
    pub allowed_headers: Option<CorsAllowHeader>,
    pub allow_credentials: Option<bool>,
    pub max_age: Option<i32>,
    pub options_passthrough: Option<bool>,
    /// Compiled form of `allowed_origins`, built on first use (never per
    /// request) and shared by clones.
    #[serde(skip)]
    pub(crate) origin_patterns: OnceLock<Vec<Option<Regex>>>,
}

impl Default for CorsConfig {
    fn default() -> Self {
        Self {
            allowed_origins: CorsAllowedOrigins::All,
            allowed_methods: Vec::new(),
            allowed_headers: None,
            allow_credentials: None,
            max_age: None,
            options_passthrough: None,
            origin_patterns: OnceLock::new(),
        }
    }
}

impl PartialEq for CorsConfig {
    fn eq(&self, other: &Self) -> bool {
        self.allowed_origins == other.allowed_origins
            && self.allowed_methods == other.allowed_methods
            && self.allowed_headers == other.allowed_headers
            && self.allow_credentials == other.allow_credentials
            && self.max_age == other.max_age
            && self.options_passthrough == other.options_passthrough
    }
}

#[async_trait]
impl Middleware for CorsConfig {
    async fn handle_response(
        &self,
        _req_path: &str,
        response: &mut Response<BoxBody<Bytes, AppError>>,
        inbound_headers: HeaderMap,
    ) -> Result<(), AppError> {
        let request_origin = inbound_headers
            .get(header::ORIGIN)
            .and_then(|value| value.to_str().ok());
        self.apply_cors_headers(response, request_origin)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum CorsAllowedOrigins {
    All,
    Origins(Vec<String>),
}
impl Serialize for CorsAllowedOrigins {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            CorsAllowedOrigins::All => serializer.serialize_str("*"),
            CorsAllowedOrigins::Origins(v) => {
                let mut seq = serializer.serialize_seq(Some(v.len()))?;
                for item in v {
                    seq.serialize_element(&item)?;
                }
                seq.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for CorsAllowedOrigins {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct CorsAllowedOriginsVisitor;

        impl<'de> Visitor<'de> for CorsAllowedOriginsVisitor {
            type Value = CorsAllowedOrigins;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a string \"*\" or a list of strings")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if value == "*" {
                    Ok(CorsAllowedOrigins::All)
                } else {
                    Err(E::custom(format!(
                        "expected '*' or list of strings, found '{value}'"
                    )))
                }
            }

            fn visit_seq<A>(self, seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let vec = Vec::deserialize(SeqAccessDeserializer::new(seq))?;
                Ok(CorsAllowedOrigins::Origins(vec))
            }
        }

        deserializer.deserialize_any(CorsAllowedOriginsVisitor)
    }
}

impl Display for CorsAllowedOrigins {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CorsAllowedOrigins::All => write!(f, "*"),
            CorsAllowedOrigins::Origins(v) => {
                write!(f, "{}", v.join(", "))
            }
        }
    }
}
impl CorsConfig {
    /// True when `origin` is allowed by `allowed_origins`.
    ///
    /// Patterns are matched once-compiled: a pattern without regex
    /// metacharacters is compared literally, anything else is compiled as an
    /// anchored regex (`^(?:pattern)$`) so that a pattern like
    /// `https://example.com` can never match `https://example.com.attacker.tld`.
    pub fn is_origin_allowed(&self, origin: &str) -> bool {
        match &self.allowed_origins {
            CorsAllowedOrigins::All => true,
            CorsAllowedOrigins::Origins(patterns) => {
                let compiled = self.origin_patterns.get_or_init(|| {
                    patterns
                        .iter()
                        .map(|pattern| compile_origin_pattern(pattern))
                        .collect()
                });
                patterns
                    .iter()
                    .zip(compiled.iter())
                    .any(|(raw, regex)| match regex {
                        Some(regex) => regex.is_match(origin),
                        None => raw == origin,
                    })
            }
        }
    }

    /// Builds the response to a CORS preflight (`OPTIONS` + `Origin` +
    /// `Access-Control-Request-Method`) request.
    ///
    /// `origin` must be the *actual* request origin: echoing it back is the only
    /// valid behaviour when `allow_credentials` is enabled.
    pub fn preflight_response(
        &self,
        origin: &str,
    ) -> Result<Response<BoxBody<Bytes, AppError>>, AppError> {
        let empty_body = || Full::new(Bytes::new()).map_err(AppError::from).boxed();

        if !self.is_origin_allowed(origin) {
            debug!("[CORS] Preflight from disallowed origin '{origin}'");
            return Ok(Response::builder()
                .status(StatusCode::FORBIDDEN)
                .body(empty_body())?);
        }

        let methods_header = self
            .allowed_methods
            .iter()
            .map(|m| m.as_str())
            .collect::<Vec<&str>>()
            .join(", ");
        let headers_header = self
            .allowed_headers
            .as_ref()
            .map(|item| item.to_string())
            .unwrap_or_default();

        let mut builder = Response::builder()
            .status(StatusCode::NO_CONTENT)
            .header(header::ACCESS_CONTROL_ALLOW_METHODS, methods_header)
            .header(header::VARY, "Origin");

        if origin.is_empty() {
            builder = builder.header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*");
        } else {
            builder = builder.header(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        }
        if !headers_header.is_empty() {
            builder = builder.header(header::ACCESS_CONTROL_ALLOW_HEADERS, headers_header);
        }
        if self.allow_credentials == Some(true) {
            builder = builder.header(
                header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
                HeaderValue::from_static("true"),
            );
        }
        if let Some(max_age) = self.max_age
            && max_age > 0
        {
            builder = builder.header(header::ACCESS_CONTROL_MAX_AGE, max_age.to_string());
        }

        Ok(builder.body(empty_body())?)
    }

    /// Adds the CORS headers to a normal (non-preflight) response.
    ///
    /// When `allowed_origins` is a whitelist, the request's `Origin` is echoed
    /// back — writing the whole list into `Access-Control-Allow-Origin` (the old
    /// behaviour) is invalid and browsers reject it.
    pub fn apply_cors_headers(
        &self,
        response: &mut Response<BoxBody<Bytes, AppError>>,
        request_origin: Option<&str>,
    ) -> Result<(), AppError> {
        let allow_origin = match &self.allowed_origins {
            CorsAllowedOrigins::All => {
                if self.allow_credentials == Some(true) {
                    request_origin.map(|origin| origin.to_string())
                } else {
                    Some("*".to_string())
                }
            }
            CorsAllowedOrigins::Origins(_) => request_origin
                .filter(|origin| self.is_origin_allowed(origin))
                .map(|origin| origin.to_string()),
        };

        let Some(allow_origin) = allow_origin else {
            debug!("[CORS] Origin not allowed, not adding CORS headers");
            return Ok(());
        };

        let headers = response.headers_mut();
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_str(allow_origin.as_str())?,
        );
        // Only a wildcard origin may be cached for every requester; an echoed
        // origin makes the response vary on the Origin request header.
        if allow_origin != "*" {
            headers.append(header::VARY, HeaderValue::from_static("Origin"));
        }

        let methods = self
            .allowed_methods
            .iter()
            .map(|m| m.as_str())
            .collect::<Vec<&str>>()
            .join(", ");
        if !methods.is_empty() {
            headers.insert(
                header::ACCESS_CONTROL_ALLOW_METHODS,
                HeaderValue::from_str(&methods)?,
            );
        }
        if let Some(cors_headers) = &self.allowed_headers {
            headers.insert(
                header::ACCESS_CONTROL_ALLOW_HEADERS,
                HeaderValue::from_str(&cors_headers.to_string())?,
            );
        }
        if self.allow_credentials == Some(true) {
            headers.insert(
                header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
                HeaderValue::from_static("true"),
            );
        }
        if let Some(max_age) = self.max_age
            && max_age > 0
        {
            headers.insert(
                header::ACCESS_CONTROL_MAX_AGE,
                HeaderValue::from_str(&max_age.to_string())?,
            );
        }
        Ok(())
    }
}

/// Returns `Some(anchored regex)` for patterns that use regex syntax, `None` for
/// plain strings that should be compared literally.
fn compile_origin_pattern(pattern: &str) -> Option<Regex> {
    let has_meta = pattern.chars().any(|c| {
        matches!(
            c,
            '^' | '$' | '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '\\'
        )
    });
    if !has_meta {
        return None;
    }
    match Regex::new(&format!("^(?:{pattern})$")) {
        Ok(regex) => Some(regex),
        Err(e) => {
            error!("Invalid CORS origin pattern '{pattern}': {e}");
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Method {
    #[serde(rename = "GET")]
    Get,
    #[serde(rename = "POST")]
    Post,
    #[serde(rename = "PUT")]
    Put,
    #[serde(rename = "DELETE")]
    Delete,
    #[serde(rename = "HEAD")]
    Head,
    #[serde(rename = "OPTIONS")]
    Options,
}
impl Method {
    pub fn as_str(&self) -> &str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Delete => "DELETE",
            Method::Head => "HEAD",
            Method::Options => "OPTIONS",
        }
    }
}
impl Serialize for CorsAllowHeader {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            CorsAllowHeader::All => serializer.serialize_str("*"),
            CorsAllowHeader::Headers(headers) => headers.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for CorsAllowHeader {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct CorsAllowHeaderVisitor;

        impl<'de> Visitor<'de> for CorsAllowHeaderVisitor {
            type Value = CorsAllowHeader;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a string '*' or an array of header names")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if value == "*" {
                    Ok(CorsAllowHeader::All)
                } else {
                    Err(de::Error::custom(r#"expected "*" or array of headers"#))
                }
            }

            fn visit_seq<A>(self, seq: A) -> Result<Self::Value, A::Error>
            where
                A: de::SeqAccess<'de>,
            {
                Vec::<HeaderName>::deserialize(de::value::SeqAccessDeserializer::new(seq))
                    .map(CorsAllowHeader::Headers)
            }
        }

        deserializer.deserialize_any(CorsAllowHeaderVisitor)
    }
}
#[derive(Debug, Clone, PartialEq)]
pub enum CorsAllowHeader {
    All,
    Headers(Vec<HeaderName>),
}

impl Display for CorsAllowHeader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CorsAllowHeader::All => write!(f, "*"),
            CorsAllowHeader::Headers(headers) => {
                let s = headers
                    .iter()
                    .map(|item| item.as_str())
                    .collect::<Vec<&str>>()
                    .join(", ");
                write!(f, "{s}")
            }
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HeaderName {
    ContentType,
    Authorization,
    Accepts,
    SetCookie,
    Cookie,
    Range,
}
impl HeaderName {
    pub fn as_str(&self) -> &str {
        match self {
            HeaderName::ContentType => "Content-Type",
            HeaderName::Authorization => "Authorization",
            HeaderName::Accepts => "Accepts",
            HeaderName::SetCookie => "Set-Cookie",
            HeaderName::Cookie => "Cookie",
            HeaderName::Range => "Range",
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cors_allowed_origins_serialize() {
        let all = CorsAllowedOrigins::All;
        let origins = CorsAllowedOrigins::Origins(vec!["http://localhost:3000".to_string()]);

        assert_eq!(serde_json::to_string(&all).unwrap(), "\"*\"");
        assert_eq!(
            serde_json::to_string(&origins).unwrap(),
            "[\"http://localhost:3000\"]"
        );
    }

    #[test]
    fn test_cors_allowed_origins_deserialize() {
        let all: CorsAllowedOrigins = serde_json::from_str("\"*\"").unwrap();
        let origins: CorsAllowedOrigins =
            serde_json::from_str("[\"http://localhost:3000\"]").unwrap();

        assert_eq!(all, CorsAllowedOrigins::All);
        assert_eq!(
            origins,
            CorsAllowedOrigins::Origins(vec!["http://localhost:3000".to_string()])
        );
    }

    #[test]
    fn test_validate_origin() {
        let config = CorsConfig {
            allowed_origins: CorsAllowedOrigins::Origins(vec!["http://localhost:\\d+".to_string()]),
            allowed_methods: vec![Method::Get],
            ..Default::default()
        };

        assert!(config.is_origin_allowed("http://localhost:3000"));
        assert!(!config.is_origin_allowed("http://example.com"));
    }

    #[test]
    fn test_origin_pattern_is_anchored() {
        let config = CorsConfig {
            allowed_origins: CorsAllowedOrigins::Origins(vec!["https://example.com".to_string()]),
            ..Default::default()
        };

        assert!(config.is_origin_allowed("https://example.com"));
        // Suffix that merely contains the configured origin used to match.
        assert!(!config.is_origin_allowed("https://example.com.attacker.tld"));
    }

    #[test]
    fn test_wildcard_allows_every_origin() {
        let config = CorsConfig {
            allowed_origins: CorsAllowedOrigins::All,
            ..Default::default()
        };
        assert!(config.is_origin_allowed("https://anything.example"));
    }

    #[test]
    fn test_preflight_echoes_request_origin() {
        let config = CorsConfig {
            allowed_origins: CorsAllowedOrigins::Origins(vec!["https://example.com".to_string()]),
            allowed_methods: vec![Method::Get, Method::Post],
            allow_credentials: Some(true),
            ..Default::default()
        };

        let response = config.preflight_response("https://example.com").unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .unwrap(),
            "https://example.com"
        );
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
                .unwrap(),
            "true"
        );
    }

    #[test]
    fn test_preflight_rejects_unknown_origin() {
        let config = CorsConfig {
            allowed_origins: CorsAllowedOrigins::Origins(vec!["https://example.com".to_string()]),
            allowed_methods: vec![Method::Get],
            allow_credentials: Some(true),
            ..Default::default()
        };

        let response = config.preflight_response("https://evil.example").unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn test_wildcard_preflight_with_credentials_is_not_forbidden() {
        // Regression test: a wildcard origin with credentials used to answer
        // every preflight with 403.
        let config = CorsConfig {
            allowed_origins: CorsAllowedOrigins::All,
            allowed_methods: vec![Method::Get],
            allow_credentials: Some(true),
            ..Default::default()
        };

        let response = config.preflight_response("https://example.com").unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .unwrap(),
            "https://example.com"
        );
    }

    #[test]
    fn test_method() {
        assert_eq!(Method::Get.as_str(), "GET");
        assert_eq!(Method::Post.as_str(), "POST");
        assert_eq!(Method::Put.as_str(), "PUT");
        assert_eq!(Method::Delete.as_str(), "DELETE");
        assert_eq!(Method::Head.as_str(), "HEAD");
        assert_eq!(Method::Options.as_str(), "OPTIONS");
    }

    #[test]
    fn test_header_name() {
        assert_eq!(HeaderName::ContentType.as_str(), "Content-Type");
        assert_eq!(HeaderName::Authorization.as_str(), "Authorization");
        assert_eq!(HeaderName::Accepts.as_str(), "Accepts");
        assert_eq!(HeaderName::SetCookie.as_str(), "Set-Cookie");
        assert_eq!(HeaderName::Cookie.as_str(), "Cookie");
        assert_eq!(HeaderName::Range.as_str(), "Range");
    }

    #[test]
    fn test_cors_allow_header() {
        let all = CorsAllowHeader::All;
        let headers =
            CorsAllowHeader::Headers(vec![HeaderName::ContentType, HeaderName::Authorization]);

        assert_eq!(serde_json::to_string(&all).unwrap(), "\"*\"");
        assert_eq!(headers.to_string(), "Content-Type, Authorization");
    }
}
