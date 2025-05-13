use crate::constants::common_constants::DEFAULT_TEMPORARY_DIR;
use crate::control_plane::lets_encrypt::lets_encrypt_certificate;
use crate::vojo::app_config::ApiService;
use crate::vojo::app_config::AppConfig;
use crate::vojo::app_config::Route;
use crate::vojo::app_config::ServiceType;

use crate::vojo::app_error::AppError;
use crate::vojo::base_response::BaseResponse;
use crate::vojo::cli::SharedConfig;
use crate::vojo::route::BaseRoute;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::delete;
use axum::routing::{get, post};
use axum::Router;
use http::header;
use prometheus::{Encoder, TextEncoder};
use std::collections::HashMap;
use std::convert::Infallible;
use std::env;
use std::net::SocketAddr;
use std::path::Path;
use tokio::io::AsyncWriteExt;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
static INTERNAL_SERVER_ERROR: &str = "Internal Server Error";

async fn get_app_config(
    State(shared_config): State<SharedConfig>,
) -> Result<impl axum::response::IntoResponse, Infallible> {
    let app_config_res = shared_config.shared_data.lock();

    if app_config_res.is_err() {
        // return Ok((
        //     axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        //     format!("No route {}", INTERNAL_SERVER_ERROR),
        // ));
        let response = Response::builder()
            .status(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
            .header(header::CONTENT_TYPE, "application/json")
            .body(format!("No route {}", INTERNAL_SERVER_ERROR))
            .unwrap();
        return Ok(response);
    }
    let data = BaseResponse {
        response_code: 0,
        response_object: app_config_res.unwrap().clone(),
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
        .body(body)
        .unwrap();

    Ok(response)
}
async fn get_prometheus_metrics() -> Result<impl axum::response::IntoResponse, Infallible> {
    let metric_families = prometheus::gather();
    let mut buffer = vec![];
    let encoder = TextEncoder::new();
    encoder.encode(&metric_families, &mut buffer).unwrap();
    Ok((
        axum::http::StatusCode::OK,
        String::from_utf8(buffer).unwrap_or(String::from("value")),
    ))
}
async fn post_app_config(
    State(shared_config): State<SharedConfig>,
    axum::extract::Json(api_service): axum::extract::Json<ApiService>,
) -> Result<impl axum::response::IntoResponse, Infallible> {
    let t = match post_app_config_with_error(shared_config, api_service).await {
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
    api_service: ApiService,
) -> Result<impl axum::response::IntoResponse, AppError> {
    let current_type = api_service.service_config.server_type.clone();
    let port = api_service.listen_port;
    if current_type == ServiceType::Https || current_type == ServiceType::Http2Tls {
        validate_tls_config(
            api_service.service_config.cert_str.clone(),
            api_service.service_config.key_str.clone(),
        )?;
    }
    let mut rw_global_lock = shared_config.shared_data.lock().unwrap();
    match rw_global_lock
        .api_service_config
        .iter_mut()
        .find(|(_, item)| item.listen_port == api_service.listen_port)
    {
        Some((_, data)) => data.service_config.routes.push(
            api_service
                .service_config
                .routes
                .first()
                .ok_or(AppError(String::from("The route is empty!")))?
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
    let json_str = serde_json::to_string(&data).unwrap();
    Ok((axum::http::StatusCode::OK, json_str))
}
async fn delete_route(
    State(shared_config): State<SharedConfig>,
    axum::extract::Path(route_id): axum::extract::Path<String>,
) -> Result<impl axum::response::IntoResponse, Infallible> {
    let mut rw_global_lock = shared_config.shared_data.lock().unwrap();
    let mut api_services = HashMap::new();
    for (port, mut api_service) in rw_global_lock.clone().api_service_config {
        api_service
            .service_config
            .routes
            .retain(|route| route.route_id != route_id);
        if !api_service.service_config.routes.is_empty() {
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
    let json_str = serde_json::to_string(&data).unwrap();
    Ok((axum::http::StatusCode::OK, json_str))
}
async fn puts_route2(
    // State(shared_config): State<SharedConfig>,
    axum::extract::Json(route): axum::extract::Json<Route>,
) -> Result<impl axum::response::IntoResponse, Infallible> {
    let shared_config = SharedConfig::from_app_config(AppConfig::default());

    let t = match put_route_with_error(shared_config, route).await {
        Ok(r) => r.into_response(),
        Err(err) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            err.to_string(),
        )
            .into_response(),
    };
    Ok(t)
}
async fn put_route(
    State(shared_config): State<SharedConfig>,
    axum::extract::Json(route_vistor): axum::extract::Json<Route>,
) -> Result<impl axum::response::IntoResponse, Infallible> {
    // let shared_config = SharedConfig::from_app_config(AppConfig::default());

    match put_route_with_error(shared_config, route_vistor).await {
        Ok(r) => Ok((axum::http::StatusCode::OK, r)),
        Err(e) => Ok((axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}
async fn put_route_with_error(
    shared_config: SharedConfig,
    route: Route,
) -> Result<String, AppError> {
    let mut rw_global_lock = shared_config.shared_data.lock().unwrap();

    let old_route = rw_global_lock
        .api_service_config
        .iter_mut()
        .flat_map(|(_, item)| item.service_config.routes.clone())
        .find(|item| item.route_id == route.route_id)
        .ok_or(AppError(String::from(
            "Can not find the route by route id!",
        )))?;

    let mut new_route = route;
    let mut new_liveness_status = &new_route.liveness_status;
    new_liveness_status = &old_route.liveness_status.clone();

    let old_base_clusters = old_route.clone().route_cluster.get_all_route().await?;
    let hashmap = old_base_clusters
        .iter()
        .map(|item| (item.endpoint.clone(), item.clone()))
        .collect::<HashMap<String, BaseRoute>>();
    let mut new_routes = new_route.route_cluster.get_all_route().await?;
    for new_base_route in new_routes.iter_mut() {
        if hashmap.clone().contains_key(&new_base_route.endpoint) {
            let old_base_route = hashmap.get(&new_base_route.endpoint).unwrap();
            let mut alive = new_base_route.is_alive;
            alive = old_base_route.is_alive;
            let mut anomaly_detection_status = &new_base_route.anomaly_detection_status;
            anomaly_detection_status = &old_base_route.anomaly_detection_status.clone();
        }
    }
    for (_, api_service) in rw_global_lock.api_service_config.iter_mut() {
        for route in api_service.service_config.routes.iter_mut() {
            if route.route_id == route.route_id {
                *route = new_route.clone();
            }
        }
    }
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
    Ok(serde_json::to_string(&data).unwrap())
}
async fn save_config_to_file(app_config: AppConfig) -> Result<(), AppError> {
    let mut data = app_config;
    let result: bool = Path::new(DEFAULT_TEMPORARY_DIR).is_dir();
    if !result {
        let path = env::current_dir().map_err(|e| AppError(e.to_string()))?;
        let absolute_path = path.join(DEFAULT_TEMPORARY_DIR);
        std::fs::create_dir_all(absolute_path).map_err(|e| AppError(e.to_string()))?;
    }

    let mut f = tokio::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open("temporary/new_silverwind_config.yml")
        .await
        .map_err(|e| AppError(e.to_string()))?;
    data.api_service_config
        .iter_mut()
        .for_each(|(_, api_service)| {
            api_service
                .service_config
                .routes
                .iter_mut()
                .for_each(|route| {
                    route.route_id = "".to_string();
                });
        });
    let api_service_str = serde_yaml::to_string(&data).map_err(|e| AppError(e.to_string()))?;
    f.write_all(api_service_str.as_bytes())
        .await
        .map_err(|e| AppError(e.to_string()))?;
    Ok(())
}
fn validate_tls_config(
    cert_pem_option: Option<String>,
    key_pem_option: Option<String>,
) -> Result<(), AppError> {
    if cert_pem_option.is_none() || key_pem_option.is_none() {
        return Err(AppError(String::from("Cert or key is none")));
    }
    let cert_pem = cert_pem_option.unwrap();
    let mut cer_reader = std::io::BufReader::new(cert_pem.as_bytes());
    let result_certs = rustls_pemfile::certs(&mut cer_reader).next();
    if result_certs.is_none() || result_certs.unwrap().is_err() {
        return Err(AppError(String::from("Can not parse the certs pem.")));
    }
    let key_pem = key_pem_option.unwrap();
    // pkcs8::PrivateKeyInfo::from_pem(key_pem.as_str());
    let key_pem_result = pkcs8::Document::from_pem(key_pem.as_str());
    if key_pem_result.is_err() {
        return Err(AppError(String::from("Can not parse the key pem.")));
    }
    Ok(())
}
pub fn get_router(shared_config: SharedConfig) -> Router {
    axum::Router::new()
        .route("/appConfig", get(get_app_config).post(post_app_config))
        .route("/metrics", get(get_prometheus_metrics))
        .route("/route/id", delete(delete_route))
        .route("/letsEncryptCertificate", post(lets_encrypt_certificate))
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .with_state(shared_config)
}
pub async fn start_control_plane(port: i32, shared_config: SharedConfig) -> Result<(), AppError> {
    let app = get_router(shared_config);

    let addr = SocketAddr::from(([0, 0, 0, 0], port as u16));

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| AppError(e.to_string()))?;
    axum::serve(listener, app)
        .await
        .map_err(|e| AppError(e.to_string()))?;
    Ok(())
}
