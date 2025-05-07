use crate::health_check::health_check_task::HealthCheck;
use crate::proxy::http1::http_proxy::HttpProxy;
use crate::proxy::http2::grpc_proxy::GrpcProxy;
use crate::proxy::tcp::tcp_proxy::TcpProxy;
use crate::vojo::app_config::{ApiService, ServiceType};
use crate::vojo::app_error::AppError;
use crate::vojo::cli::SharedConfig;

use tokio::sync::mpsc;

pub async fn init(shared_config: SharedConfig) -> Result<(), AppError> {
    let cloned_config = shared_config.clone();
    tokio::task::spawn(async {
        let mut health_check = HealthCheck::from_shared_config(cloned_config);
        health_check.start_health_check_loop().await;
    });
    let mut app_config = shared_config
        .shared_data
        .lock()
        .map_err(|e| AppError(e.to_string()))?
        .clone();
    for (_, item) in app_config.api_service_config.iter_mut() {
        let mut api_service = item.clone();
        let port = api_service.listen_port;
        let server_type = api_service.service_config.server_type;
        let mapping_key = format!("{}-{}", port, server_type);
        let (sender, receiver) = mpsc::channel::<()>(1000);
        api_service.sender = sender;

        start_proxy(
            shared_config.clone(),
            port,
            receiver,
            server_type,
            mapping_key,
            item.clone(),
        )
        .await?;
    }
    Ok(())
}

pub async fn start_proxy(
    shared_config: SharedConfig,
    port: i32,
    channel: mpsc::Receiver<()>,
    server_type: ServiceType,
    mapping_key: String,
    apiservice: ApiService,
) -> Result<(), AppError> {
    if server_type == ServiceType::Http {
        let mut http_proxy = HttpProxy {
            shared_config,
            port,
            channel,
            mapping_key: mapping_key.clone(),
        };
        http_proxy.start_http_server().await
    } else if server_type == ServiceType::Https {
        let key_clone = mapping_key.clone();
        let service_config = apiservice.service_config;
        let pem_str = service_config
            .cert_str
            .ok_or(AppError("Pem is null.".to_string()))?;
        let key_str = service_config
            .key_str
            .ok_or(AppError("Pem is null.".to_string()))?;
        let mut http_proxy = HttpProxy {
            shared_config,
            port,
            channel,
            mapping_key: mapping_key.clone(),
        };
        http_proxy.start_https_server(pem_str, key_str).await
    } else if server_type == ServiceType::Tcp {
        let mut tcp_proxy = TcpProxy {
            shared_config,
            port,
            mapping_key,
            channel,
        };
        tcp_proxy.start_proxy().await
    } else if server_type == ServiceType::Http2 {
        let mut grpc_proxy = GrpcProxy {
            shared_config,
            port,
            mapping_key,
            channel,
        };
        grpc_proxy.start_proxy().await
    } else {
        let key_clone = mapping_key.clone();
        let service_config = apiservice.service_config;
        let pem_str = service_config.cert_str.unwrap();
        let key_str = service_config.key_str.unwrap();
        let mut grpc_proxy = GrpcProxy {
            shared_config,
            port,
            mapping_key,
            channel,
        };
        grpc_proxy.start_tls_proxy(pem_str, key_str).await
    }
}
