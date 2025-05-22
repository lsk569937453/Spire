use crate::vojo::app_config::AppConfig;
use std::sync::PoisonError;
use thiserror::Error;
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("Error is: {0}")]
pub struct AppError(pub String);
impl From<&str> for AppError {
    fn from(s: &str) -> Self {
        AppError(s.to_string())
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
