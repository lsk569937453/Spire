use super::app_error::AppError;
use regex::Regex;
use serde::de::value::SeqAccessDeserializer;
use serde::de::SeqAccess;
use serde::de::Visitor;
use serde::ser::SerializeSeq;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::Serializer;

use serde::de;
use std::fmt;

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

impl CorsAllowedOrigins {
    pub fn to_string(&self) -> String {
        match self {
            CorsAllowedOrigins::All => "*".to_string(),
            CorsAllowedOrigins::Origins(origin_list) => {
                origin_list.first().unwrap_or(&"".to_string()).to_string()
            }
        }
    }
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
impl CorsAllowHeader {
    pub fn to_string(&self) -> String {
        match self {
            CorsAllowHeader::All => "*".to_string(),
            CorsAllowHeader::Headers(headers) => headers
                .iter()
                .map(|item| item.as_str())
                .collect::<Vec<&str>>()
                .join(", "),
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
