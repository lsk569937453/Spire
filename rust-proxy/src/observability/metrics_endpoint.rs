//! Prometheus exposition endpoint of the read-only observability listener.
//!
//! Metrics used to be served by the control plane behind the admin token, which
//! meant a scrape without that token answered 401. The observability listener is
//! read-only, so it needs no token; it defaults to loopback for the same reason.
use crate::vojo::app_error::AppError;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use prometheus::{Encoder, TextEncoder};

pub async fn get_prometheus_metrics() -> Result<impl IntoResponse, AppError> {
    let metric_families = prometheus::gather();
    let mut buffer = vec![];
    let encoder = TextEncoder::new();
    encoder
        .encode(&metric_families, &mut buffer)
        .unwrap_or_default();
    Ok((
        StatusCode::OK,
        String::from_utf8(buffer).unwrap_or_default(),
    ))
}
