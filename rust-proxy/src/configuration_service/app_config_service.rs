use crate::app_error;
use crate::health_check::health_check_task::HealthCheck;
use crate::proxy::http1::http_proxy::HttpProxy;
use crate::proxy::http2::grpc_proxy::GrpcProxy;
use crate::proxy::tcp::tcp_proxy::TcpProxy;
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
        let server_type = item.server_type.clone();
        let mapping_key = format!("{}-{}", port, server_type);
        let (sender, receiver) = mpsc::channel::<()>(1000);
        item.sender = sender;
        let cloned_config = shared_config.clone();
        let cert_str = item.cert_str.clone();
        let key_str = item.key_str.clone();
        tokio::task::spawn(async move {
            if let Err(err) = start_proxy(
                cloned_config,
                port,
                receiver,
                server_type,
                mapping_key,
                cert_str,
                key_str,
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
    cert_str: Option<String>,
    key_str: Option<String>,
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
#[cfg(test)]
mod tests {
    use super::*; // Import items from the parent module
    use crate::vojo::app_config::{
        ApiService, AppConfig, ServiceConfig, ServiceType, StaticConfig,
    };
    use crate::vojo::cli::SharedConfig;

    use std::collections::HashMap;
    use std::time::Duration;
    use tokio::sync::mpsc;
    #[tokio::test]
    async fn test_start_proxy_http() {
        let shared_config = SharedConfig::from_app_config(AppConfig::default());
        let (tx, rx) = mpsc::channel(1);
        let service_config = ServiceConfig {
            server_type: ServiceType::Http,
            ..Default::default()
        };

        let proxy_task = tokio::spawn(start_proxy(
            shared_config,
            8080,
            rx,
            ServiceType::Http,
            "test-http".to_string(),
            service_config,
        ));

        tokio::time::sleep(Duration::from_millis(10)).await; // Give it time to start
        tx.send(()).await.expect("Failed to send shutdown signal");
        let result = proxy_task.await.expect("Proxy task panicked");
        assert!(result.is_ok(), "Expected Ok, got {:?}", result);
    }
    #[tokio::test]
    async fn test_start_proxy_https_success() {
        let shared_config = SharedConfig::from_app_config(AppConfig::default());
        let (tx, rx) = mpsc::channel(1);
        let service_config = ServiceConfig {
            server_type: ServiceType::Https,
            cert_str: Some("dummy_pem_content".to_string()),
            key_str: Some("dummy_key_content".to_string()),
            route_configs: vec![],
        };

        let proxy_task = tokio::spawn(start_proxy(
            shared_config,
            8081,
            rx,
            ServiceType::Https,
            "test-https".to_string(),
            service_config,
        ));
        tokio::time::sleep(Duration::from_millis(10)).await;
        let cc = tx.send(()).await;
        println!("{:?}", cc);
        let result = proxy_task.await.expect("Proxy task panicked");
        assert!(result.is_err(), "Expected Ok, got {:?}", result);
    }

    #[tokio::test]
    async fn test_start_proxy_https_missing_cert() {
        let shared_config = SharedConfig::from_app_config(AppConfig::default());
        let (_tx, rx) = mpsc::channel(1); // tx not used as it should fail before listening
        let service_config = ServiceConfig {
            server_type: ServiceType::Https,
            cert_str: None,
            key_str: Some("dummy_key_content".to_string()),
            route_configs: vec![],
        };

        // No need to spawn, call directly as it should return error quickly
        let result = start_proxy(
            shared_config,
            8082,
            rx,
            ServiceType::Https,
            "test-https-fail".to_string(),
            service_config,
        )
        .await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), AppError("Pem is null.".to_string()));
    }

    #[tokio::test]
    async fn test_start_proxy_tcp() {
        let shared_config = SharedConfig::from_app_config(AppConfig::default());
        let (tx, rx) = mpsc::channel(1);
        let service_config = ServiceConfig {
            server_type: ServiceType::Tcp,
            ..Default::default()
        };

        let proxy_task = tokio::spawn(start_proxy(
            shared_config,
            8083,
            rx,
            ServiceType::Tcp,
            "test-tcp".to_string(),
            service_config,
        ));
        tokio::time::sleep(Duration::from_millis(10)).await;
        tx.send(()).await.expect("Failed to send shutdown signal");
        let result = proxy_task.await.expect("Proxy task panicked");
        assert!(result.is_ok(), "Expected Ok, got {:?}", result);
    }

    #[tokio::test]
    async fn test_start_proxy_http2() {
        let shared_config = SharedConfig::from_app_config(AppConfig::default());
        let (tx, rx) = mpsc::channel(1);
        let service_config = ServiceConfig {
            server_type: ServiceType::Http2,
            ..Default::default()
        };

        let proxy_task = tokio::spawn(start_proxy(
            shared_config,
            8084,
            rx,
            ServiceType::Http2,
            "test-http2".to_string(),
            service_config,
        ));
        tokio::time::sleep(Duration::from_millis(10)).await;
        tx.send(()).await.expect("Failed to send shutdown signal");
        let result = proxy_task.await.expect("Proxy task panicked");
        assert!(result.is_ok(), "Expected Ok, got {:?}", result);
    }

    #[tokio::test]
    async fn test_start_proxy_grpc_tls_success() {
        let shared_config = SharedConfig::from_app_config(AppConfig::default());
        let (tx, rx) = mpsc::channel(1);
        let service_config = ServiceConfig {
            server_type: ServiceType::Http2Tls,
            cert_str: Some("dummy_pem_content".to_string()),
            key_str: Some("dummy_key_content".to_string()),
            route_configs: vec![],
        };

        let proxy_task = tokio::spawn(start_proxy(
            shared_config,
            8085,
            rx,
            ServiceType::Http2Tls,
            "test-grpc-tls".to_string(),
            service_config,
        ));
        tokio::time::sleep(Duration::from_millis(10)).await;
        let tt = tx.send(()).await;
        println!("{:?}", tt);
        let result = proxy_task.await.expect("Proxy task panicked");
        assert!(result.is_err(), "Expected Ok, got {:?}", result);
    }

    #[tokio::test]
    async fn test_start_proxy_grpc_tls_missing_key() {
        let shared_config = SharedConfig::from_app_config(AppConfig::default());
        let (_tx, rx) = mpsc::channel(1);
        let service_config = ServiceConfig {
            server_type: ServiceType::Http2Tls,
            cert_str: Some("dummy_pem_content".to_string()),
            key_str: None,
            route_configs: vec![],
        };

        let result = start_proxy(
            shared_config,
            8086,
            rx,
            ServiceType::Http2Tls,
            "test-grpc-tls-fail".to_string(),
            service_config,
        )
        .await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), AppError("Pem is null.".to_string()));
    }

    #[tokio::test]
    async fn test_init_function() {
        let services_to_init = vec![(
            "http_service".to_string(),
            9001,
            ServiceConfig {
                server_type: ServiceType::Http,
                ..Default::default()
            },
        )];
        let shared_config = SharedConfig::from_app_config(AppConfig {
            static_config: StaticConfig::default(),
            api_service_config: HashMap::from([(
                9001,
                ApiService {
                    api_service_id: "".to_string(),
                    listen_port: 9001,
                    service_config: ServiceConfig {
                        server_type: ServiceType::Http,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )]),
        });

        let init_result = init(shared_config.clone()).await;
        assert!(init_result.is_ok());

        let app_config_guard = shared_config.shared_data.lock().unwrap();
        for (service_name, port, service_conf) in &services_to_init {
            let api_service = app_config_guard
                .api_service_config
                .get(&9001)
                .expect("Service not found in config after init");
            assert_eq!(api_service.listen_port, *port);
            assert_eq!(
                api_service.service_config.server_type,
                service_conf.server_type
            );
        }
        drop(app_config_guard);

        tokio::time::sleep(Duration::from_millis(100)).await;
        println!("test_init_function completed.");
    }
}
