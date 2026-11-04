use crate::constants::common_constants::MAX_ADMIN_BODY_BYTES;
use crate::control_plane::certificate_api::lets_encrypt_certificate;
use crate::vojo::app_config::ApiService;
use crate::vojo::app_config::AppConfig;
use crate::vojo::app_config::RouteConfig;
use axum::Json;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::extract::Request;
use axum::extract::State;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::delete;
use axum::routing::put;
use axum::routing::{get, post};
use http::header;
use prometheus::{Encoder, TextEncoder};
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::OnceLock;
use std::time::Instant;
use subtle::ConstantTimeEq;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use crate::vojo::app_error::AppError;
use crate::vojo::base_response::BaseResponse;
use crate::vojo::cli::SharedConfig;

/// Serialises concurrent writes to the configuration file.
fn save_lock() -> &'static Mutex<()> {
    static SAVE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    SAVE_LOCK.get_or_init(|| Mutex::new(()))
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.ct_eq(right).into()
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({ "error": "missing or invalid admin token" })),
    )
        .into_response()
}

/// Requires `Authorization: Bearer <admin_token>` (or `X-Admin-Token`) whenever
/// `admin_token` is configured.
///
/// The control plane can read and rewrite the whole configuration, so it must
/// never be reachable without credentials — binding to loopback is the default,
/// this token is the second line of defence.
#[allow(clippy::result_large_err)]
async fn require_admin_token(
    State(shared_config): State<SharedConfig>,
    req: Request,
    next: Next,
) -> Result<Response, Response> {
    let expected = {
        let config = shared_config
            .shared_data
            .read()
            .map_err(|_| unauthorized())?;
        config.admin_token.clone()
    };

    let Some(expected) = expected else {
        // No token configured: only acceptable because the default bind address
        // is loopback. Warn loudly if that is not the case.
        return Ok(next.run(req).await);
    };

    let provided = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::to_string)
        .or_else(|| {
            req.headers()
                .get("x-admin-token")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        });

    match provided {
        Some(provided) if constant_time_eq(provided.as_bytes(), expected.as_bytes()) => {
            Ok(next.run(req).await)
        }
        _ => Err(unauthorized()),
    }
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn get_app_config(
    State(shared_config): State<SharedConfig>,
) -> Result<impl IntoResponse, AppError> {
    let app_config = shared_config.shared_data.read()?.clone();
    Ok(Json(BaseResponse {
        response_code: 0,
        response_object: app_config,
    }))
}

async fn get_prometheus_metrics() -> Result<impl IntoResponse, AppError> {
    let metric_families = prometheus::gather();
    let mut buffer = vec![];
    let encoder = TextEncoder::new();
    encoder
        .encode(&metric_families, &mut buffer)
        .unwrap_or_default();
    Ok((
        axum::http::StatusCode::OK,
        String::from_utf8(buffer).unwrap_or_default(),
    ))
}

async fn read_body(req: Request) -> Result<bytes::Bytes, AppError> {
    let (_, body) = req.into_parts();
    // Bounded: `usize::MAX` here was a one-request memory exhaustion vector.
    Ok(axum::body::to_bytes(body, MAX_ADMIN_BODY_BYTES).await?)
}

async fn post_app_config(
    State(shared_config): State<SharedConfig>,
    req: Request,
) -> Result<impl IntoResponse, AppError> {
    let bytes = read_body(req).await?;
    let api_service: ApiService = serde_yaml::from_slice(&bytes)?;
    let port = api_service.listen_port;

    let app_config = {
        let mut config = shared_config.shared_data.write()?;
        match config
            .api_service_config
            .iter_mut()
            .find(|(_, item)| item.listen_port == port)
        {
            Some((_, data)) => data.route_configs.push(
                api_service
                    .route_configs
                    .first()
                    .ok_or(AppError::from("The route is empty!"))?
                    .clone(),
            ),
            None => {
                config.api_service_config.insert(port, api_service);
            }
        };
        config.clone()
    };

    persist(&shared_config, &app_config).await?;

    Ok(Json(BaseResponse {
        response_code: 0,
        response_object: 0,
    }))
}

async fn delete_route(
    State(shared_config): State<SharedConfig>,
    axum::extract::Path(route_id): axum::extract::Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let app_config = {
        let mut config = shared_config.shared_data.write()?;
        let mut removed = false;
        for api_service in config.api_service_config.values_mut() {
            let before = api_service.route_configs.len();
            api_service
                .route_configs
                .retain(|route| route.route_id != route_id);
            removed |= before != api_service.route_configs.len();
        }
        if !removed {
            return Err(AppError(format!("Can not find the route '{route_id}'")));
        }
        // A listener whose routes all disappeared is kept (with no routes) so
        // that the set of listen ports stays stable across a reload.
        config.clone()
    };

    persist(&shared_config, &app_config).await?;

    Ok(Json(BaseResponse {
        response_code: 0,
        response_object: 0,
    }))
}

async fn put_route(
    State(shared_config): State<SharedConfig>,
    req: Request,
) -> Result<impl IntoResponse, AppError> {
    let bytes = read_body(req).await?;
    let route: RouteConfig = serde_yaml::from_slice(&bytes)?;

    let app_config = {
        let mut config = shared_config.shared_data.write()?;
        let old_route = config
            .api_service_config
            .values_mut()
            .flat_map(|item| item.route_configs.iter_mut())
            .find(|r| r.route_id == route.route_id)
            .ok_or(AppError::from("Can not find the route by route id!"))?;
        *old_route = route;
        config.clone()
    };

    persist(&shared_config, &app_config).await?;

    Ok(Json(BaseResponse {
        response_code: 0,
        response_object: 0,
    }))
}

async fn reload_config(
    State(shared_config): State<SharedConfig>,
    req: Request,
) -> Result<impl IntoResponse, AppError> {
    let bytes = read_body(req).await?;
    let new_config: AppConfig = serde_yaml::from_slice(&bytes)?;

    let app_config = {
        let mut config = shared_config.shared_data.write()?;

        // The reload keeps the existing listeners, so the set of ports must not
        // change (otherwise the new services would silently never be served).
        let current_ports: std::collections::BTreeSet<i32> =
            config.api_service_config.keys().copied().collect();
        let new_ports: std::collections::BTreeSet<i32> =
            new_config.api_service_config.keys().copied().collect();
        if current_ports != new_ports {
            return Err(AppError(format!(
                "Port mismatch: current ports {:?} do not match new ports {:?}. \
                The reload operation requires the exact same set of listen ports.",
                current_ports, new_ports
            )));
        }

        // Keep the live shutdown channels: they belong to the running listeners,
        // not to the file the operator just uploaded.
        let mut new_config = new_config;
        for (port, service) in new_config.api_service_config.iter_mut() {
            if let Some(running) = config.api_service_config.get(port) {
                service.sender = running.sender.clone();
            }
        }
        *config = new_config;
        config.clone()
    };

    persist(&shared_config, &app_config).await?;

    Ok(Json(BaseResponse {
        response_code: 0,
        response_object: "Configuration reloaded successfully",
    }))
}

/// Writes the configuration back to the file it was loaded from.
///
/// * the file is written to a temporary sibling and then renamed, so a crash can
///   never leave a truncated configuration behind;
/// * route ids are preserved (they used to be blanked, which made `PUT /route`
///   and `DELETE /route/{id}` unusable after a restart).
async fn persist(shared_config: &SharedConfig, app_config: &AppConfig) -> Result<(), AppError> {
    let _guard = save_lock().lock().await;
    save_config_to_file(app_config, shared_config.config_path.as_path()).await
}

async fn save_config_to_file(app_config: &AppConfig, path: &Path) -> Result<(), AppError> {
    let serialized = serde_yaml::to_string(app_config)?;

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent).await?;
    }

    let tmp_path = path.with_extension("yaml.tmp");
    {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp_path)
            .await?;
        file.write_all(serialized.as_bytes()).await?;
        file.sync_all().await?;
    }
    tokio::fs::rename(&tmp_path, path).await?;
    Ok(())
}

async fn print_request_response(req: Request<axum::body::Body>, next: Next) -> Response {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let start = Instant::now();

    let response = next.run(req).await;

    let duration = start.elapsed();
    debug!(
        "{} {} {} {} {:?}",
        method,
        uri.path(),
        uri.query().unwrap_or(""),
        response.status(),
        duration
    );

    response
}

pub fn get_router(shared_config: SharedConfig) -> Router {
    // Every route here can read or rewrite the configuration, so the token check
    // applies to all of them.
    let protected = axum::Router::new()
        .route("/appConfig", get(get_app_config).post(post_app_config))
        // Documented alias for the read endpoint.
        .route("/config", get(get_app_config))
        .route("/metrics", get(get_prometheus_metrics))
        .route("/route/{id}", delete(delete_route))
        .route("/route", put(put_route))
        .route("/letsEncryptCertificate", post(lets_encrypt_certificate))
        .route("/reload", post(reload_config))
        .layer(axum::middleware::from_fn_with_state(
            shared_config.clone(),
            require_admin_token,
        ));

    axum::Router::new()
        // Liveness probe: stays unauthenticated (it exposes nothing) so that
        // container/load-balancer health checks work without a token.
        .route("/health", get(health))
        .merge(protected)
        .layer(axum::middleware::from_fn(print_request_response))
        .layer(DefaultBodyLimit::max(MAX_ADMIN_BODY_BYTES))
        .with_state(shared_config)
}

pub async fn start_control_plane(
    port: i32,
    shared_config: SharedConfig,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), AppError> {
    let listen = shared_config.shared_data.read()?.admin_listen_addr();
    let ip: IpAddr = listen
        .parse()
        .map_err(|e| AppError(format!("Invalid admin_listen address '{listen}': {e}")))?;
    let addr = SocketAddr::new(ip, port as u16);

    let app = get_router(shared_config.clone());

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| AppError(format!("Failed to bind control plane on {addr}: {e}")))?;
    info!("Control plane listening on http://{addr}");

    if !ip.is_loopback() {
        let token_configured = shared_config
            .shared_data
            .read()
            .map(|config| config.admin_token.is_some())
            .unwrap_or(false);
        if !token_configured {
            warn!(
                "The control plane is exposed on {addr} without an 'admin_token'. \
                 Anyone who can reach this port can read and rewrite the whole configuration."
            );
        }
    }

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::control_plane::rest_api::Router;
    use crate::control_plane::rest_api::get_router;
    use crate::control_plane::rest_api::save_config_to_file;
    use crate::vojo::app_config::ApiService;
    use crate::vojo::app_config::RouteConfig;

    use crate::vojo::base_response::BaseResponse;

    use crate::vojo::app_config::AppConfig;
    use crate::vojo::cli::SharedConfig;
    use axum::body::Body;
    use axum::extract::Request;
    use http::StatusCode;
    use http::header;
    use std::collections::HashMap;
    use tempfile::TempDir;
    use tower::ServiceExt;

    fn setup() -> (Router, SharedConfig, TempDir) {
        let dir = tempfile::tempdir().expect("temp dir");
        let config_path = dir.path().join("config.yaml");

        let initial_route = RouteConfig {
            route_id: "route1".to_string(),
            ..Default::default()
        };

        let initial_api_service = ApiService {
            listen_port: 8080,
            route_configs: vec![initial_route],
            ..Default::default()
        };

        let mut api_service_config = HashMap::new();
        api_service_config.insert(8080, initial_api_service);

        let app_config = AppConfig {
            api_service_config,
            ..Default::default()
        };

        let shared_config = SharedConfig::from_app_config_with_path(app_config, config_path);

        (get_router(shared_config.clone()), shared_config, dir)
    }

    fn setup_with_token(token: &str) -> (Router, SharedConfig, TempDir) {
        let (_router, shared_config, dir) = setup();
        {
            let mut config = shared_config.shared_data.write().unwrap();
            config.admin_token = Some(token.to_string());
        }
        (get_router(shared_config.clone()), shared_config, dir)
    }

    #[tokio::test]
    async fn test_get_app_config_success() {
        let (router, shared_config, _dir) = setup();

        let request = Request::builder()
            .uri("/appConfig")
            .method("GET")
            .body(Body::empty())
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_response: BaseResponse<AppConfig> = serde_json::from_slice(&body).unwrap();

        let expected_config = shared_config.shared_data.read().unwrap().clone();
        assert_eq!(body_response.response_object, expected_config);
    }

    #[tokio::test]
    async fn test_config_is_not_served_without_the_admin_token() {
        let (router, _shared_config, _dir) = setup_with_token("s3cret");

        let request = Request::builder()
            .uri("/appConfig")
            .method("GET")
            .body(Body::empty())
            .unwrap();
        let response = router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let wrong_token = Request::builder()
            .uri("/appConfig")
            .method("GET")
            .header(header::AUTHORIZATION, "Bearer wrong")
            .body(Body::empty())
            .unwrap();
        let response = router.clone().oneshot(wrong_token).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let request = Request::builder()
            .uri("/appConfig")
            .method("GET")
            .header(header::AUTHORIZATION, "Bearer s3cret")
            .body(Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_post_app_config_new_service() {
        let (router, shared_config, _dir) = setup();

        let new_route = RouteConfig {
            route_id: "new_route".to_string(),
            ..Default::default()
        };

        let new_api_service = ApiService {
            listen_port: 9090,
            route_configs: vec![new_route],
            ..Default::default()
        };

        let yaml_body = serde_yaml::to_string(&new_api_service).unwrap();

        let request = Request::builder()
            .uri("/appConfig")
            .method("POST")
            .header(header::CONTENT_TYPE, "application/yaml")
            .body(Body::from(yaml_body))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let locked_config = shared_config.shared_data.read().unwrap();
        assert_eq!(locked_config.api_service_config.len(), 2);
        assert!(locked_config.api_service_config.contains_key(&9090));
    }

    #[tokio::test]
    async fn test_post_app_config_add_route_to_existing_service() {
        let (router, shared_config, _dir) = setup();

        let new_route = RouteConfig {
            route_id: "route2".to_string(),
            ..Default::default()
        };

        let api_service_update = ApiService {
            listen_port: 8080,
            route_configs: vec![new_route],
            ..Default::default()
        };

        let yaml_body = serde_yaml::to_string(&api_service_update).unwrap();

        let request = Request::builder()
            .uri("/appConfig")
            .method("POST")
            .header(header::CONTENT_TYPE, "application/yaml")
            .body(Body::from(yaml_body))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let locked_config = shared_config.shared_data.read().unwrap();
        let service_8080 = locked_config.api_service_config.get(&8080).unwrap();
        assert_eq!(service_8080.route_configs.len(), 2);
        assert!(
            service_8080
                .route_configs
                .iter()
                .any(|r| r.route_id == "route1")
        );
        assert!(
            service_8080
                .route_configs
                .iter()
                .any(|r| r.route_id == "route2")
        );
    }

    #[tokio::test]
    async fn test_put_route_success() {
        let (router, shared_config, _dir) = setup();

        let updated_route = RouteConfig {
            route_id: "route1".to_string(), // Same ID
            path_rewrite: Some("/rewritten".to_string()),
            ..Default::default()
        };

        let yaml_body = serde_yaml::to_string(&updated_route).unwrap();

        let request = Request::builder()
            .uri("/route")
            .method("PUT")
            .header(header::CONTENT_TYPE, "application/yaml")
            .body(Body::from(yaml_body))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let locked_config = shared_config.shared_data.read().unwrap();
        let service_8080 = locked_config.api_service_config.get(&8080).unwrap();
        let route = service_8080.route_configs.first().unwrap();
        assert_eq!(route.path_rewrite.as_deref(), Some("/rewritten"));
    }

    #[tokio::test]
    async fn test_put_route_not_found() {
        let (router, _shared_config, _dir) = setup();

        let non_existent_route = RouteConfig {
            route_id: "non-existent-route".to_string(),
            ..Default::default()
        };

        let yaml_body = serde_yaml::to_string(&non_existent_route).unwrap();

        let request = Request::builder()
            .uri("/route")
            .method("PUT")
            .header(header::CONTENT_TYPE, "application/yaml")
            .body(Body::from(yaml_body))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let error_message = String::from_utf8(body.to_vec()).unwrap();
        assert!(error_message.contains("Can not find the route by route id!"));
    }

    #[tokio::test]
    async fn test_delete_route_success() {
        let (router, shared_config, _dir) = setup();

        let request = Request::builder()
            .uri("/route/route1")
            .method("DELETE")
            .body(Body::empty())
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let locked_config = shared_config.shared_data.read().unwrap();
        // The listener is kept (with no routes) so the port set stays stable.
        let service = locked_config.api_service_config.get(&8080).unwrap();
        assert!(service.route_configs.is_empty());
    }

    #[tokio::test]
    async fn test_get_prometheus_metrics_success() {
        let (router, _shared_config, _dir) = setup();

        let counter = prometheus::IntCounter::new("test_metric", "A test metric").unwrap();
        prometheus::register(Box::new(counter.clone())).unwrap();
        counter.inc();

        let request = Request::builder()
            .uri("/metrics")
            .method("GET")
            .body(Body::empty())
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();

        assert!(body_str.contains("# HELP test_metric A test metric"));
        assert!(body_str.contains("# TYPE test_metric counter"));
        assert!(body_str.contains("test_metric 1"));

        prometheus::unregister(Box::new(counter)).unwrap();
    }

    #[tokio::test]
    async fn test_health_endpoint() {
        let (router, _shared_config, _dir) = setup();
        let request = Request::builder()
            .uri("/health")
            .method("GET")
            .body(Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_save_config_to_file_keeps_route_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");

        let app_config = AppConfig {
            api_service_config: HashMap::from([(
                8080,
                ApiService {
                    listen_port: 8080,
                    route_configs: vec![RouteConfig {
                        route_id: "keep-me".to_string(),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };

        save_config_to_file(&app_config, &path).await.unwrap();

        let written = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(
            written.contains("keep-me"),
            "route ids must survive a persist, got: {written}"
        );

        // No temporary file may be left behind.
        assert!(!path.with_extension("yaml.tmp").exists());

        // And it must round-trip.
        let reparsed: AppConfig = serde_yaml::from_str(&written).unwrap();
        assert_eq!(reparsed, app_config);
    }

    #[tokio::test]
    async fn test_reload_rejects_port_changes() {
        let (router, _shared_config, _dir) = setup();

        let mut new_config = AppConfig::default();
        new_config.api_service_config.insert(
            1234,
            ApiService {
                listen_port: 1234,
                ..Default::default()
            },
        );
        let yaml_body = serde_yaml::to_string(&new_config).unwrap();

        let request = Request::builder()
            .uri("/reload")
            .method("POST")
            .body(Body::from(yaml_body))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
