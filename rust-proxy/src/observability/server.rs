//! Read-only observability listener: `GET /health` and `GET /metrics`.
//!
//! This replaces the control-plane HTTP API that used to be able to read and
//! rewrite the whole configuration over HTTP. Nothing here can write: the
//! configuration is only ever read from disk, which is what makes a
//! file-composed configuration (`include`) safe to run.
use crate::observability::health::health;
use crate::observability::metrics_endpoint::get_prometheus_metrics;
use crate::vojo::app_config::ObservabilityConfig;
use crate::vojo::app_error::AppError;
use axum::Router;
use axum::routing::get;
use std::future::Future;
use std::net::SocketAddr;
use tokio::task::JoinHandle;

pub fn get_router() -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/metrics", get(get_prometheus_metrics))
}

/// Binds and serves the observability listener.
///
/// Returns `None` when the listener is disabled by configuration. The bind
/// happens before the task is spawned so that a port conflict fails startup
/// instead of silently leaving the process without a metrics endpoint.
pub async fn start_observability(
    config: &ObservabilityConfig,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<Option<JoinHandle<()>>, AppError> {
    if !config.enabled {
        info!("Observability listener disabled by configuration.");
        return Ok(None);
    }

    let addr: SocketAddr = config.listen.parse().map_err(|e| {
        AppError(format!(
            "Invalid 'observability.listen' address '{}': {e}",
            config.listen
        ))
    })?;

    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| {
        AppError(format!(
            "Failed to bind the observability listener on {addr}: {e}"
        ))
    })?;

    if !addr.ip().is_loopback() {
        warn!(
            "The observability listener is exposed on {addr}; /health and /metrics are \
             unauthenticated (read-only) and should sit behind a firewall."
        );
    }
    info!("Observability listener on http://{addr} (/health, /metrics)");

    let app = get_router();
    let handle = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app)
            .with_graceful_shutdown(shutdown)
            .await
        {
            error!("Observability listener stopped: {e}");
        }
    });
    Ok(Some(handle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::StatusCode;
    use http::Request;
    use tower::ServiceExt;

    #[tokio::test]
    async fn router_serves_health_and_metrics() {
        let app = get_router();

        let health = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(health.status(), StatusCode::OK);

        let metrics = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(metrics.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn disabled_listener_does_not_bind() {
        let config = ObservabilityConfig {
            enabled: false,
            ..Default::default()
        };
        let handle = start_observability(&config, std::future::pending::<()>())
            .await
            .expect("disabled listener must not fail");
        assert!(handle.is_none());
    }
}
