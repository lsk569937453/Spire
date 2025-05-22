use crate::vojo::app_config::AppConfig;
use http::header::InvalidHeaderValue;
use http::header::ToStrError;
use http::uri::InvalidUriParts;
use rustls_pki_types::InvalidDnsNameError;
use std::sync::PoisonError;
use std::time::SystemTimeError;
use thiserror::Error;
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("Error is: {0}")]
pub struct AppError(pub String);
impl From<&str> for AppError {
    fn from(s: &str) -> Self {
        AppError(s.to_string())
    }
}
impl From<InvalidHeaderValue> for AppError {
    fn from(error: InvalidHeaderValue) -> Self {
        AppError(format!("Invalid header value: {}", error))
    }
}
impl From<hyper_util::client::legacy::Error> for AppError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        AppError(format!("Hyper client error: {}", error))
    }
}
impl From<http::uri::InvalidUri> for AppError {
    fn from(error: http::uri::InvalidUri) -> Self {
        AppError(format!("Invalid URI: {}", error))
    }
}
impl From<InvalidUriParts> for AppError {
    fn from(error: InvalidUriParts) -> Self {
        AppError(format!("Invalid URI parts: {}", error))
    }
}
impl From<ipnet::AddrParseError> for AppError {
    fn from(error: ipnet::AddrParseError) -> Self {
        AppError(format!("IP address parse error: {}", error))
    }
}
impl From<SystemTimeError> for AppError {
    fn from(error: SystemTimeError) -> Self {
        AppError(format!("System time error: {}", error))
    }
}
impl From<InvalidDnsNameError> for AppError {
    fn from(error: InvalidDnsNameError) -> Self {
        AppError(format!("Invalid DNS name error: {}", error))
    }
}
impl From<rcgen::Error> for AppError {
    fn from(error: rcgen::Error) -> Self {
        AppError(format!("Certificate generation error: {}", error))
    }
}
impl From<instant_acme::Error> for AppError {
    fn from(error: instant_acme::Error) -> Self {
        AppError(format!("Instant ACME error: {}", error))
    }
}
impl From<regex::Error> for AppError {
    fn from(error: regex::Error) -> Self {
        AppError(format!("Regex error: {}", error))
    }
}
impl From<std::net::AddrParseError> for AppError {
    fn from(error: std::net::AddrParseError) -> Self {
        AppError(format!("Address parse error: {}", error))
    }
}

impl From<h2::Error> for AppError {
    fn from(error: h2::Error) -> Self {
        AppError(format!("H2 error: {}", error))
    }
}
impl From<hyper::Error> for AppError {
    fn from(error: hyper::Error) -> Self {
        AppError(format!("Hyper error: {}", error))
    }
}
impl From<axum::Error> for AppError {
    fn from(error: axum::Error) -> Self {
        AppError(format!("Axum error: {}", error))
    }
}
impl From<ToStrError> for AppError {
    fn from(error: ToStrError) -> Self {
        AppError(format!("Header to string error: {}", error))
    }
}
impl From<serde_yaml::Error> for AppError {
    fn from(error: serde_yaml::Error) -> Self {
        AppError(format!("YAML error: {}", error))
    }
}
impl From<std::io::Error> for AppError {
    fn from(error: std::io::Error) -> Self {
        AppError(format!("IO error: {}", error))
    }
}
impl From<url::ParseError> for AppError {
    fn from(error: url::ParseError) -> Self {
        AppError(format!("URL parse error: {}", error))
    }
}
impl From<rustls::Error> for AppError {
    fn from(error: rustls::Error) -> Self {
        AppError(format!("TLS error: {}", error))
    }
}
impl From<http::Error> for AppError {
    fn from(error: http::Error) -> Self {
        AppError(format!("HTTP error: {}", error))
    }
}
impl From<PoisonError<std::sync::MutexGuard<'_, AppConfig>>> for AppError {
    fn from(error: PoisonError<std::sync::MutexGuard<'_, AppConfig>>) -> Self {
        AppError(format!("Mutex error: {}", error))
    }
}
impl From<serde_json::Error> for AppError {
    fn from(error: serde_json::Error) -> Self {
        AppError(format!("JSON error: {}", error))
    }
}
