//! Liveness probe served by the read-only observability listener.
//!
//! Unauthenticated on purpose: it exposes nothing, and container / load-balancer
//! health checks must work without credentials.
use axum::http::StatusCode;
use axum::response::IntoResponse;

pub async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::routing::get;
    use http::Request;
    use tower::ServiceExt;

    #[tokio::test]
    async fn health_returns_ok() {
        let app = Router::new().route("/health", get(health));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
    }
}
