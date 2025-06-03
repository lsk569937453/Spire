use crate::constants::common_constants::DEFAULT_TEMPORARY_DIR;
use crate::control_plane::lets_encrypt::lets_encrypt_certificate;
use crate::vojo::app_config::ApiService;
use crate::vojo::app_config::AppConfig;
use crate::vojo::app_config::RouteConfig;
use crate::vojo::app_config::ServiceType;
use axum::middleware::Next;
use std::time::Instant;

use crate::vojo::app_error::AppError;
use crate::vojo::base_response::BaseResponse;
use crate::vojo::cli::SharedConfig;
use axum::extract::Request;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::delete;
use axum::routing::put;
use axum::routing::{get, post};
use axum::Json;
use axum::Router;
use http::header;
use prometheus::{Encoder, TextEncoder};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use tokio::io::AsyncWriteExt;
use tower_http::cors::CorsLayer;
static INTERNAL_SERVER_ERROR: &str = "Internal Server Error";

async fn get_app_config(
    State(shared_config): State<SharedConfig>,
) -> Result<impl axum::response::IntoResponse, Infallible> {
    match get_app_config_with_error(shared_config).await {
        Ok(response) => Ok(response),
        Err(err) => {
            let response = Response::builder()
                .status(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                .header(header::CONTENT_TYPE, "application/json")
                .body(format!("Error is : {}", err))
                .unwrap_or_default();
            Ok(response)
        }
    }
}
async fn get_app_config_with_error(
    shared_config: SharedConfig,
) -> Result<Response<String>, AppError> {
    let app_config_res = shared_config.shared_data.lock()?;

    let data = BaseResponse {
        response_code: 0,
        response_object: app_config_res.clone(),
    };
    let (status, body) = match serde_yaml::to_string(&data) {
        Ok(json) => (axum::http::StatusCode::OK, json),
        Err(_) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("No route {}", INTERNAL_SERVER_ERROR),
        ),
    };
    let response = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body)?;
    Ok(response)
}

async fn get_prometheus_metrics() -> Result<impl axum::response::IntoResponse, AppError> {
    let metric_families = prometheus::gather();
    let mut buffer = vec![];
    let encoder = TextEncoder::new();
    encoder
        .encode(&metric_families, &mut buffer)
        .unwrap_or_default();
    Ok((
        axum::http::StatusCode::OK,
        String::from_utf8(buffer).unwrap_or(String::from("value")),
    ))
}
async fn post_app_config(
    State(shared_config): State<SharedConfig>,
    req: Request,
) -> Result<impl axum::response::IntoResponse, Infallible> {
    let t = match post_app_config_with_error(shared_config, req).await {
        Ok(r) => r.into_response(),
        Err(err) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            err.to_string(),
        )
            .into_response(),
    };
    Ok(t)
}
async fn post_app_config_with_error(
    shared_config: SharedConfig,
    req: Request,
) -> Result<impl axum::response::IntoResponse, AppError> {
    let (_, body) = req.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX).await?;
    let api_service: ApiService = serde_yaml::from_slice(&bytes)?;
    let current_type = api_service.service_config.server_type.clone();
    let port = api_service.listen_port;
    if current_type == ServiceType::Https || current_type == ServiceType::Http2Tls {
        validate_tls_config(api_service.cert_str.clone(), api_service.key_str.clone())?;
    }
    let mut rw_global_lock = shared_config.shared_data.lock()?;
    match rw_global_lock
        .api_service_config
        .iter_mut()
        .find(|(_, item)| item.listen_port == api_service.listen_port)
    {
        Some((_, data)) => data.service_config.route_configs.push(
            api_service
                .service_config
                .route_configs
                .first()
                .ok_or(AppError::from("The route is empty!"))?
                .clone(),
        ),
        None => {
            rw_global_lock.api_service_config.insert(port, api_service);
        }
    };
    let app_config = rw_global_lock.clone();
    tokio::spawn(async {
        if let Err(err) = save_config_to_file(app_config).await {
            error!("Save file error,the error is {}!", err);
        }
    });
    let data = BaseResponse {
        response_code: 0,
        response_object: 0,
    };
    let json_str = serde_json::to_string(&data)?;

    let response = Response::builder()
        .status(axum::http::StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(json_str)?;

    Ok(response)
}

async fn delete_route(
    State(shared_config): State<SharedConfig>,
    axum::extract::Path(route_id): axum::extract::Path<String>,
) -> Result<impl axum::response::IntoResponse, Infallible> {
    match delete_route_with_error(shared_config, route_id).await {
        Ok(r) => Ok((axum::http::StatusCode::OK, r)),
        Err(err) => Ok((
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            err.to_string(),
        )),
    }
}
async fn delete_route_with_error(
    shared_config: SharedConfig,
    route_id: String,
) -> Result<String, AppError> {
    let mut rw_global_lock = shared_config.shared_data.lock()?;
    let mut api_services = HashMap::new();
    for (port, mut api_service) in rw_global_lock.clone().api_service_config {
        api_service
            .service_config
            .route_configs
            .retain(|route| route.route_id != route_id);
        if !api_service.service_config.route_configs.is_empty() {
            api_services.insert(port, api_service);
        }
    }
    rw_global_lock.api_service_config = api_services;

    let app_config = rw_global_lock.clone();
    tokio::spawn(async {
        if let Err(err) = save_config_to_file(app_config).await {
            error!("Save file error,the error is {}!", err);
        }
    });

    let data = BaseResponse {
        response_code: 0,
        response_object: 0,
    };
    let json_str = serde_json::to_string(&data)?;
    Ok(json_str)
}
async fn put_routex(
    State(shared_config): State<SharedConfig>,
    req: Request,
) -> Result<impl axum::response::IntoResponse, Infallible> {
    let t = match put_route_with_error(shared_config, req).await {
        Ok(r) => r.into_response(),
        Err(err) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            err.to_string(),
        )
            .into_response(),
    };
    Ok(t)
}
async fn put_route_with_error(
    shared_config: SharedConfig,
    req: Request,
) -> Result<String, AppError> {
    let (_, body) = req.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX).await?;
    let route: RouteConfig = serde_yaml::from_slice(&bytes)?;
    let mut rw_global_lock = shared_config.shared_data.lock()?;

    let old_route = rw_global_lock
        .api_service_config
        .iter_mut()
        .flat_map(|(_, item)| item.service_config.route_configs.iter_mut())
        .find(|r| r.route_id == route.route_id)
        .ok_or(AppError(String::from(
            "Can not find the route by route id!",
        )))?;

    *old_route = route;

    let app_config = rw_global_lock.clone();
    tokio::spawn(async {
        if let Err(err) = save_config_to_file(app_config).await {
            error!("Save file error,the error is {}!", err);
        }
    });
    let data = BaseResponse {
        response_code: 0,
        response_object: 0,
    };
    Ok(serde_json::to_string(&data)?)
}
async fn save_config_to_file(app_config: AppConfig) -> Result<(), AppError> {
    let mut data = app_config;
    let result: bool = Path::new(DEFAULT_TEMPORARY_DIR).is_dir();
    if !result {
        let path = env::current_dir()?;
        let absolute_path = path.join(DEFAULT_TEMPORARY_DIR);
        std::fs::create_dir_all(absolute_path)?;
    }

    let mut f = tokio::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open("temporary/new_spire_config.yml")
        .await?;
    data.api_service_config
        .iter_mut()
        .for_each(|(_, api_service)| {
            api_service
                .service_config
                .route_configs
                .iter_mut()
                .for_each(|route| {
                    route.route_id = "".to_string();
                });
        });
    let api_service_str = serde_yaml::to_string(&data)?;
    f.write_all(api_service_str.as_bytes()).await?;
    Ok(())
}
pub fn validate_tls_config(
    cert_pem_option: Option<String>,
    key_pem_option: Option<String>,
) -> Result<(), AppError> {
    if cert_pem_option.is_none() || key_pem_option.is_none() {
        return Err(AppError::from("Cert or key is none"));
    }
    let cert_pem = cert_pem_option.ok_or(AppError(String::from("Cert is none")))?;
    let mut cer_reader = std::io::BufReader::new(cert_pem.as_bytes());
    let result_certs = rustls_pemfile::certs(&mut cer_reader).next();
    if result_certs.is_none()
        || result_certs
            .ok_or(AppError("result_certs is null".to_string()))?
            .is_err()
    {
        return Err(AppError(String::from("Can not parse the certs pem.")));
    }
    let key_pem = key_pem_option.ok_or(AppError(String::from("Key is none")))?;
    let key_pem_result = pkcs8::Document::from_pem(key_pem.as_str());
    if key_pem_result.is_err() {
        return Err(AppError::from("Can not parse the key pem."));
    }
    Ok(())
}
async fn print_request_response(req: Request<axum::body::Body>, next: Next) -> Response {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let start = Instant::now();

    // 处理请求
    let response = next.run(req).await;

    // 记录日志
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
    axum::Router::new()
        .route("/appConfig", get(get_app_config).post(post_app_config))
        .route("/metrics", get(get_prometheus_metrics))
        .route("/route/{id}", delete(delete_route))
        .route("/route", put(put_routex))
        .route("/letsEncryptCertificate", post(lets_encrypt_certificate))
        .layer(axum::middleware::from_fn(print_request_response))
        .layer(CorsLayer::permissive())
        .with_state(shared_config)
}
pub async fn start_control_plane(port: i32, shared_config: SharedConfig) -> Result<(), AppError> {
    let app = get_router(shared_config);

    let addr = SocketAddr::from(([0, 0, 0, 0], port as u16));

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use crate::control_plane::rest_api::get_router;
    use crate::control_plane::rest_api::save_config_to_file;
    use crate::control_plane::rest_api::Router;
    use crate::control_plane::rest_api::DEFAULT_TEMPORARY_DIR;
    use crate::vojo::app_config::ApiService;
    use crate::vojo::app_config::RouteConfig;

    use crate::vojo::base_response::BaseResponse;
    use crate::AppConfig;
    use crate::SharedConfig;
    use axum::body::Body;
    use axum::extract::Request;
    use http::header;

    use http::StatusCode;
    use std::collections::HashMap;
    use tower::ServiceExt;
    fn setup() -> (Router, SharedConfig) {
        let _ = std::fs::remove_dir_all(DEFAULT_TEMPORARY_DIR);

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

        let shared_config = SharedConfig::from_app_config(app_config);

        (get_router(shared_config.clone()), shared_config)
    }

    fn cleanup() {
        let _ = std::fs::remove_dir_all(DEFAULT_TEMPORARY_DIR);
    }
    #[tokio::test]
    async fn test_get_app_config_success() {
        let (router, shared_config) = setup();

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
        println!("{}", String::from_utf8_lossy(&body));
        let body_response: BaseResponse<AppConfig> = serde_yaml::from_slice(&body).unwrap();

        let expected_config = shared_config.shared_data.lock().unwrap().clone();
        assert_eq!(body_response.response_object, expected_config);

        cleanup();
    }

    #[tokio::test]
    async fn test_post_app_config_new_service() {
        let (router, shared_config) = setup();

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
        let responsexx = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        println!("{}", String::from_utf8_lossy(&responsexx));
        // assert_eq!(response.status(), StatusCode::OK);

        let locked_config = shared_config.shared_data.lock().unwrap();
        assert_eq!(locked_config.api_service_config.len(), 2);
        assert!(locked_config.api_service_config.contains_key(&9090));
        assert_eq!(
            locked_config
                .api_service_config
                .get(&9090)
                .unwrap()
                .listen_port,
            9090
        );

        cleanup();
    }

    #[tokio::test]
    async fn test_post_app_config_add_route_to_existing_service() {
        let (router, shared_config) = setup();

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

        let locked_config = shared_config.shared_data.lock().unwrap();
        let service_8080 = locked_config.api_service_config.get(&8080).unwrap();
        assert_eq!(service_8080.route_configs.len(), 2);
        assert!(service_8080
            .route_configs
            .iter()
            .any(|r| r.route_id == "route1"));
        assert!(service_8080
            .route_configs
            .iter()
            .any(|r| r.route_id == "route2"));

        cleanup();
    }

    #[tokio::test]
    async fn test_put_route_success() {
        let (router, shared_config) = setup();

        let updated_route = RouteConfig {
            route_id: "route1".to_string(), // Same ID
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

        let locked_config = shared_config.shared_data.lock().unwrap();
        let service_8080 = locked_config.api_service_config.get(&8080).unwrap();
        let _route = service_8080.route_configs.first().unwrap();

        cleanup();
    }

    #[tokio::test]
    async fn test_put_route_not_found() {
        let (router, _) = setup();

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

        cleanup();
    }

    #[tokio::test]
    async fn test_delete_route_success() {
        let (router, shared_config) = setup();

        let request = Request::builder()
            .uri("/route/route1")
            .method("DELETE")
            .body(Body::empty())
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let locked_config = shared_config.shared_data.lock().unwrap();
        assert!(locked_config.api_service_config.is_empty());

        cleanup();
    }

    #[tokio::test]
    async fn test_get_prometheus_metrics_success() {
        let (router, _) = setup();

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
        cleanup();
    }
    #[tokio::test]
    async fn test_save_config_to_file_success() {
        let app_config = AppConfig::default();
        let res = save_config_to_file(app_config).await;
        println!("{:?}", res);
        assert!(res.is_ok());
    }
    use crate::control_plane::rest_api::validate_tls_config;
    #[tokio::test]
    async fn test_validate_tls_config_success() {
        let cert_pem = "-----BEGIN CERTIFICATE-----...-----END CERTIFICATE-----";
        let key_pem = "-----BEGIN PRIVATE KEY-----...-----END PRIVATE KEY-----";
        let res = validate_tls_config(Some(cert_pem.to_string()), Some(key_pem.to_string()));
        assert!(res.is_err());
    }
}
