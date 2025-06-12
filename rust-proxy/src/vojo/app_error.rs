use crate::vojo::app_config::AppConfig;
use axum::response::IntoResponse;
use axum::response::Response;
use http::header::InvalidHeaderValue;
use http::header::ToStrError;
use http::uri::InvalidUriParts;
use http::StatusCode;
use rustls_pki_types::InvalidDnsNameError;
use std::sync::PoisonError;
use std::time::SystemTimeError;
use thiserror::Error;
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("Error is: {0}")]
pub struct AppError(pub String);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, error_message) = (StatusCode::INTERNAL_SERVER_ERROR, self.to_string());

        error!("Error processing request: {}", self);

        (status, error_message).into_response()
    }
}
#[macro_export]
macro_rules! app_error {
    ($($arg:tt)*) => {
        AppError(format!($($arg)*))
    }
}
impl From<&str> for AppError {
    fn from(s: &str) -> Self {
        AppError(s.to_string())
    }
}
impl From<std::convert::Infallible> for AppError {
    fn from(_: std::convert::Infallible) -> Self {
        AppError("Infallible error".to_string())
    }
}
impl From<std::num::ParseIntError> for AppError {
    fn from(error: std::num::ParseIntError) -> Self {
        AppError(format!("Parse int error: {}", error))
    }
}
impl From<delay_timer::error::TaskError> for AppError {
    fn from(error: delay_timer::error::TaskError) -> Self {
        AppError(format!("Task error: {}", error))
    }
}
impl From<tracing_subscriber::util::TryInitError> for AppError {
    fn from(error: tracing_subscriber::util::TryInitError) -> Self {
        AppError(format!(
            "Tracing subscriber initialization error: {}",
            error
        ))
    }
}
impl From<tracing_appender::rolling::InitError> for AppError {
    fn from(error: tracing_appender::rolling::InitError) -> Self {
        AppError(format!(
            "Rolling file appender initialization error: {}",
            error
        ))
    }
}
impl From<tokio::time::error::Elapsed> for AppError {
    fn from(error: tokio::time::error::Elapsed) -> Self {
        AppError(format!("Timeout error: {}", error))
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
#[cfg(test)]
mod tests {
    use super::*;

    use std::io::ErrorKind;
    use std::sync::Arc;
    use std::time::Duration;
    use std::time::SystemTime;

    fn create_parse_int_error() -> std::num::ParseIntError {
        "abc".parse::<i32>().unwrap_err()
    }

    async fn create_elapsed_error() -> tokio::time::error::Elapsed {
        tokio::time::timeout(
            Duration::from_millis(1),
            tokio::time::sleep(Duration::from_millis(100)),
        )
        .await
        .unwrap_err()
    }

    fn create_invalid_header_value() -> InvalidHeaderValue {
        http::header::HeaderValue::from_bytes(b"\0").unwrap_err()
    }

    fn create_invalid_uri_error() -> http::uri::InvalidUri {
        " ".parse::<http::Uri>().unwrap_err()
    }

    fn create_invalid_uri_parts_error() -> InvalidUriParts {
        let mut bad_parts = http::uri::Parts::default();
        bad_parts.scheme = Some("http".parse().unwrap());
        bad_parts.authority = None;

        bad_parts.path_and_query = Some("foo/bar".parse().unwrap());
        http::Uri::from_parts(bad_parts).unwrap_err()
    }

    fn create_ipnet_addr_parse_error() -> ipnet::AddrParseError {
        "invalid".parse::<ipnet::IpNet>().unwrap_err()
    }

    fn create_system_time_error() -> SystemTimeError {
        SystemTime::UNIX_EPOCH
            .duration_since(SystemTime::now() + Duration::from_secs(1000))
            .unwrap_err()
    }

    fn create_invalid_dns_name_error() -> InvalidDnsNameError {
        InvalidDnsNameError
    }

    fn create_rcgen_error() -> rcgen::Error {
        rcgen::Error::CouldNotParseCertificate
    }

    fn create_instant_acme_error() -> instant_acme::Error {
        instant_acme::Error::Str("()")
    }
    #[allow(clippy::invalid_regex)]
    fn create_regex_error() -> regex::Error {
        regex::Regex::new("[").unwrap_err()
    }

    fn create_std_net_addr_parse_error() -> std::net::AddrParseError {
        "invalid-addr".parse::<std::net::SocketAddr>().unwrap_err()
    }

    fn create_h2_error() -> h2::Error {
        h2::Error::from(h2::Reason::INTERNAL_ERROR)
    }

    fn create_axum_error() -> axum::Error {
        let io_err = std::io::Error::other("axum test io error");
        axum::Error::new(io_err)
    }

    fn create_to_str_error() -> ToStrError {
        let hv = http::header::HeaderValue::from_bytes(b"invalid\xFF").unwrap();
        hv.to_str().unwrap_err()
    }

    fn create_serde_yaml_error() -> serde_yaml::Error {
        serde_yaml::from_str::<serde_json::Value>("key: [unclosed").unwrap_err()
    }

    fn create_io_error() -> std::io::Error {
        std::io::Error::new(ErrorKind::NotFound, "test io error")
    }

    fn create_url_parse_error() -> url::ParseError {
        url::ParseError::EmptyHost
    }

    fn create_rustls_error() -> rustls::Error {
        rustls::Error::NoApplicationProtocol
    }

    fn create_http_error() -> http::Error {
        http::Request::builder()
            .method("INVALID METHOD")
            .body(())
            .unwrap_err()
    }

    fn create_serde_json_error() -> serde_json::Error {
        serde_json::from_str::<serde_json::Value>("{ malformed: json }").unwrap_err()
    }

    #[test]
    fn test_from_str() {
        let s = "a basic error message";
        let app_error: AppError = s.into();
        assert_eq!(app_error.0, "a basic error message");
        assert_eq!(app_error.to_string(), "Error is: a basic error message");
    }

    #[test]
    fn test_from_parse_int_error() {
        let original_error = create_parse_int_error();
        let expected_message = format!("Parse int error: {}", original_error);
        let app_error: AppError = original_error.into();
        assert_eq!(app_error.0, expected_message);
        assert_eq!(
            app_error.to_string(),
            format!("Error is: {}", expected_message)
        );
    }
    #[test]
    fn test_from_delay_timer_task_error() {
        let original_error = delay_timer::error::TaskError::DisGetEvent(
            delay_timer::prelude::channel::TryRecvError::Closed,
        );
        let expected_message = format!("Task error: {}", original_error);
        let app_error: AppError = original_error.into();
        assert_eq!(app_error.0, expected_message);
    }

    #[tokio::test]
    async fn test_from_tokio_time_elapsed() {
        let original_error = create_elapsed_error().await;
        let expected_message = format!("Timeout error: {}", original_error);
        let app_error: AppError = original_error.into();
        assert_eq!(app_error.0, expected_message);
    }

    #[test]
    fn test_from_invalid_header_value() {
        let original_error = create_invalid_header_value();
        let expected_message = format!("Invalid header value: {}", original_error);
        let app_error: AppError = original_error.into();
        assert_eq!(app_error.0, expected_message);
    }

    #[test]
    fn test_from_http_uri_invalid_uri() {
        let original_error = create_invalid_uri_error();
        let expected_message = format!("Invalid URI: {}", original_error);
        let app_error: AppError = original_error.into();
        assert_eq!(app_error.0, expected_message);
    }

    #[test]
    fn test_from_invalid_uri_parts() {
        let original_error = create_invalid_uri_parts_error();
        let expected_message = format!("Invalid URI parts: {}", original_error);
        let app_error: AppError = original_error.into();
        assert_eq!(app_error.0, expected_message);
    }

    #[test]
    fn test_from_ipnet_addr_parse_error() {
        let original_error = create_ipnet_addr_parse_error();
        let expected_message = format!("IP address parse error: {}", original_error);
        let app_error: AppError = original_error.into();
        assert_eq!(app_error.0, expected_message);
    }

    #[test]
    fn test_from_system_time_error() {
        let original_error = create_system_time_error();
        let expected_message = format!("System time error: {}", original_error);
        let app_error: AppError = original_error.into();
        assert_eq!(app_error.0, expected_message);
    }

    #[test]
    fn test_from_invalid_dns_name_error() {
        let original_error = create_invalid_dns_name_error();
        let expected_message = format!("Invalid DNS name error: {}", original_error);
        let app_error: AppError = original_error.into();
        assert_eq!(app_error.0, expected_message);
    }

    #[test]
    fn test_from_rcgen_error() {
        let original_error = create_rcgen_error();
        let expected_message = format!("Certificate generation error: {}", original_error);
        let app_error: AppError = original_error.into();
        assert_eq!(app_error.0, expected_message);
    }

    #[test]
    fn test_from_instant_acme_error() {
        let original_error = create_instant_acme_error();
        let expected_message = format!("Instant ACME error: {}", original_error);
        let app_error: AppError = original_error.into();
        assert_eq!(app_error.0, expected_message);
    }

    #[test]
    fn test_from_regex_error() {
        let original_error = create_regex_error();
        let expected_message = format!("Regex error: {}", original_error);
        let app_error: AppError = original_error.into();
        assert_eq!(app_error.0, expected_message);
    }

    #[test]
    fn test_from_std_net_addr_parse_error() {
        let original_error = create_std_net_addr_parse_error();
        let expected_message = format!("Address parse error: {}", original_error);
        let app_error: AppError = original_error.into();
        assert_eq!(app_error.0, expected_message);
    }

    #[test]
    fn test_from_h2_error() {
        let original_error = create_h2_error();
        let expected_message = format!("H2 error: {}", original_error);
        let app_error: AppError = original_error.into();
        assert_eq!(app_error.0, expected_message);
    }

    #[test]
    fn test_from_axum_error() {
        let original_error = create_axum_error();
        let expected_message = format!("Axum error: {}", original_error);
        let app_error: AppError = original_error.into();
        assert_eq!(app_error.0, expected_message);
    }

    #[test]
    fn test_from_to_str_error() {
        let original_error = create_to_str_error();
        let expected_message = format!("Header to string error: {}", original_error);
        let app_error: AppError = original_error.into();
        assert_eq!(app_error.0, expected_message);
    }

    #[test]
    fn test_from_serde_yaml_error() {
        let original_error = create_serde_yaml_error();
        let expected_message = format!("YAML error: {}", original_error);
        let app_error: AppError = original_error.into();
        assert_eq!(app_error.0, expected_message);
    }

    #[test]
    fn test_from_io_error() {
        let original_error = create_io_error();
        let expected_message = format!("IO error: {}", original_error);
        let app_error: AppError = original_error.into();
        assert_eq!(app_error.0, expected_message);
    }

    #[test]
    fn test_from_url_parse_error() {
        let original_error = create_url_parse_error();
        let expected_message = format!("URL parse error: {}", original_error);
        let app_error: AppError = original_error.into();
        assert_eq!(app_error.0, expected_message);
    }

    #[test]
    fn test_from_rustls_error() {
        let original_error = create_rustls_error();
        let expected_message = format!("TLS error: {}", original_error);
        let app_error: AppError = original_error.into();
        assert_eq!(app_error.0, expected_message);
    }

    #[test]
    fn test_from_http_error() {
        let original_error = create_http_error();
        let expected_message = format!("HTTP error: {}", original_error);
        let app_error: AppError = original_error.into();
        assert_eq!(app_error.0, expected_message);
    }

    #[test]
    fn test_from_poison_error_app_config() {
        let app_config_data = Arc::new(std::sync::Mutex::new(AppConfig::default()));
        let app_config_data_clone = Arc::clone(&app_config_data);

        let handle = std::thread::spawn(move || {
            let _guard = app_config_data_clone.lock().unwrap();
            panic!("Simulated panic while holding AppConfig lock");
        });

        assert!(handle.join().is_err());

        let original_error = app_config_data.lock().unwrap_err();
        let expected_message = format!("Mutex error: {}", original_error);
        let app_error: AppError = original_error.into();
        assert_eq!(app_error.0, expected_message);
    }

    #[test]
    fn test_from_serde_json_error() {
        let original_error = create_serde_json_error();
        let expected_message = format!("JSON error: {}", original_error);
        let app_error: AppError = original_error.into();
        assert_eq!(app_error.0, expected_message);
    }
}
