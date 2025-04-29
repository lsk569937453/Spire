use crate::configuration_service::logger;
use crate::constants;
use crate::constants::common_constants::ENV_ACCESS_LOG;
use crate::constants::common_constants::ENV_ADMIN_PORT;
use crate::constants::common_constants::ENV_CONFIG_FILE_PATH;
use crate::constants::common_constants::ENV_DATABASE_URL;
use crate::constants::common_constants::TIMER_WAIT_SECONDS;
use crate::health_check::health_check_task::HealthCheck;
use crate::proxy::http1::http_proxy::HttpProxy;
use crate::proxy::http2::grpc_proxy::GrpcProxy;
use crate::proxy::tcp::tcp_proxy::TcpProxy;
use crate::vojo::api_service_manager::ApiServiceManager;
use crate::vojo::app_config::ServiceConfig;
use crate::vojo::app_config::{ApiService, AppConfig, ServiceType};
use crate::vojo::app_config_vistor::ApiServiceVistor;
use crate::vojo::app_error::AppError;
use crate::vojo::cli::SharedConfig;
use dashmap::DashMap;
use futures::FutureExt;
use lazy_static::lazy_static;
use log::Level;
use std::collections::HashMap;
use std::env;
use tokio::sync::mpsc;
use tokio::sync::RwLock;
use tokio::time::sleep;

pub async fn init(shared_config: SharedConfig) ->Result<(),AppError>{
    tokio::task::spawn(async {
        let mut health_check = HealthCheck::new();
        health_check.start_health_check_loop().await;
    });
    let app_config=shared_config.shared_data.lock()?;
    for item in app_config.
    Ok(())
}

pub async fn start_proxy(
    port: i32,
    channel: mpsc::Receiver<()>,
    server_type: ServiceType,
    mapping_key: String,
) -> Result<(), AppError> {
    if server_type == ServiceType::Http {
        let mut http_proxy = HttpProxy {
            port,
            channel,
            mapping_key: mapping_key.clone(),
        };
        http_proxy.start_http_server().await
    } else if server_type == ServiceType::Https {
        let key_clone = mapping_key.clone();
        let service_config = GLOBAL_CONFIG_MAPPING
            .get(&key_clone)
            .unwrap()
            .service_config
            .clone();
        let pem_str = service_config.cert_str.unwrap();
        let key_str = service_config.key_str.unwrap();
        let mut http_proxy = HttpProxy {
            port,
            channel,
            mapping_key: mapping_key.clone(),
        };
        http_proxy.start_https_server(pem_str, key_str).await
    } else if server_type == ServiceType::Tcp {
        let mut tcp_proxy = TcpProxy {
            port,
            mapping_key,
            channel,
        };
        tcp_proxy.start_proxy().await
    } else if server_type == ServiceType::Http2 {
        let mut grpc_proxy = GrpcProxy {
            port,
            mapping_key,
            channel,
        };
        grpc_proxy.start_proxy().await
    } else {
        let key_clone = mapping_key.clone();
        let service_config = GLOBAL_CONFIG_MAPPING
            .get(&key_clone)
            .unwrap()
            .service_config
            .clone();
        let pem_str = service_config.cert_str.unwrap();
        let key_str = service_config.key_str.unwrap();
        let mut grpc_proxy = GrpcProxy {
            port,
            mapping_key,
            channel,
        };
        grpc_proxy.start_tls_proxy(pem_str, key_str).await
    }
}
