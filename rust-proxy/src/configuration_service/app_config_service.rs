use crate::health_check::health_check_task::HealthCheck;
use crate::proxy::http1::http_proxy::HttpProxy;
use crate::proxy::http2::grpc_proxy::GrpcProxy;
use crate::proxy::tcp::tcp_proxy::TcpProxy;
use crate::vojo::app_config::ServiceConfig;
use crate::vojo::app_config::ServiceType;
use crate::vojo::app_error::AppError;
use crate::vojo::cli::SharedConfig;
use tokio::sync::mpsc;

pub async fn init(shared_config: SharedConfig) -> Result<(), AppError> {
    let cloned_config = shared_config.clone();
    tokio::task::spawn(async {
        let mut health_check = HealthCheck::from_shared_config(cloned_config);
        health_check.start_health_check_loop().await;
    });
    let mut app_config = shared_config.shared_data.lock()?;
    for (_, item) in app_config.api_service_config.iter_mut() {
        let port = item.listen_port;
        let server_type = item.service_config.server_type.clone();
        let mapping_key = format!("{}-{}", port, server_type);
        let (sender, receiver) = mpsc::channel::<()>(1000);
        item.sender = sender;
        let cloned_config = shared_config.clone();
        let service_config = item.service_config.clone();
        tokio::task::spawn(async move {
            if let Err(err) = start_proxy(
                cloned_config,
                port,
                receiver,
                server_type,
                mapping_key,
                service_config,
            )
            .await
            {
                error!("{}", err);
            }
        });
    }
    Ok(())
}

pub async fn start_proxy(
    shared_config: SharedConfig,
    port: i32,
    channel: mpsc::Receiver<()>,
    server_type: ServiceType,
    mapping_key: String,
    service_config: ServiceConfig,
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
        let pem_str = service_config
            .cert_str
            .ok_or(AppError("Pem is null.".to_string()))?;
        let key_str = service_config
            .key_str
            .ok_or(AppError("Pem is null.".to_string()))?;
        let mut grpc_proxy = GrpcProxy {
            shared_config,
            port,
            mapping_key,
            channel,
        };
        grpc_proxy.start_tls_proxy(pem_str, key_str).await
    }
}
