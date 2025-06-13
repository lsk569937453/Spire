use crate::vojo::app_error::AppError;
use bytes::Bytes;
use http::header;
use http::HeaderValue;
use http::Response;
use http_body_util::combinators::BoxBody;
use regex::Regex;
use serde::de;
use serde::de::value::SeqAccessDeserializer;
use serde::de::SeqAccess;
use serde::de::Visitor;
use serde::ser::SerializeSeq;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::Serializer;
use std::fmt;
use std::fmt::Display;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CorsConfig {
    pub allowed_origins: CorsAllowedOrigins,
    pub allowed_methods: Vec<Method>,
    pub allowed_headers: Option<CorsAllowHeader>,
    pub allow_credentials: Option<bool>,
    pub max_age: Option<i32>,
    pub options_passthrough: Option<bool>,
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
                        "expected '*' or list of strings, found '{}'",
                        value
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
impl CorsAllowedOrigins {
    pub fn is_all(&self) -> bool {
        match self {
            CorsAllowedOrigins::All => true,
            CorsAllowedOrigins::Origins(_) => false,
        }
    }
}
impl CorsConfig {
    pub fn validate_origin(&self, origin: &str) -> Result<bool, AppError> {
        match &self.allowed_origins {
            CorsAllowedOrigins::All => Ok(self.allow_credentials.unwrap_or(true)),
            CorsAllowedOrigins::Origins(allowed_origins) => {
                for allowed_origin in allowed_origins {
                    let regex = Regex::new(allowed_origin.as_str())?;
                    if regex.is_match(origin) {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
        }
    }
    pub fn handle_before_response(
        &self,
        response: &mut Response<BoxBody<Bytes, AppError>>,
    ) -> Result<(), AppError> {
        let headers = response.headers_mut();
        let origin = self.allowed_origins.to_string();
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_str(origin.as_str())?,
        );

        let methods = self
            .allowed_methods
            .iter()
            .map(|m| m.as_str().to_uppercase())
            .collect::<Vec<String>>()
            .join(", ");
        info!("methods: {}", methods);
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_str(&methods)?,
        );
        if let Some(cors_headers) = &self.allowed_headers {
            headers.insert(
                header::ACCESS_CONTROL_ALLOW_HEADERS,
                HeaderValue::from_str(&cors_headers.to_string())?,
            );
        }
        if let Some(allow_credentials) = self.allow_credentials {
            if allow_credentials {
                headers.insert(
                    header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
                    HeaderValue::from_static("true"),
                );
            }
        }
        if let Some(max_age) = self.max_age {
            if max_age > 0 {
                headers.insert(
                    header::ACCESS_CONTROL_MAX_AGE,
                    HeaderValue::from_str(&max_age.to_string())?,
                );
            }
        }

        if !self.allowed_origins.is_all() {
            headers.append(header::VARY, HeaderValue::from_static("Origin"));
        }

        Ok(())
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
                write!(f, "{}", s)
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
            allowed_headers: None,
            allow_credentials: Some(true),
            max_age: None,
            options_passthrough: None,
        };

        assert!(config.validate_origin("http://localhost:3000").unwrap());
        assert!(!config.validate_origin("http://example.com").unwrap());
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
