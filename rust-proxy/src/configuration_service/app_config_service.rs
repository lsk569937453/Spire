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
        let pem_str = service_config.cert_str.ok_or(app_error!(
            "Certificate (cert_str) is missing for TLS service on port {}",
            port
        ))?;
        let key_str = service_config.key_str.ok_or(app_error!(
            "Private key (key_str) is missing for TLS service on port {}",
            port
        ))?;
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
        let pem_str = service_config.cert_str.ok_or(app_error!(
            "Certificate (cert_str) is missing for TLS service on port {}",
            port
        ))?;
        let key_str = service_config.key_str.ok_or(app_error!(
            "Private key (key_str) is missing for TLS service on port {}",
            port
        ))?;
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
    use crate::vojo::app_config::{ApiService, AppConfig, ServiceConfig, ServiceType};
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
        let cert = r#"-----BEGIN CERTIFICATE-----
MIIDoTCCAomgAwIBAgIUXs7QoXvLAbQIvmDT09/v43EfDJowDQYJKoZIhvcNAQEL
BQAwYDELMAkGA1UEBhMCQ04xCzAJBgNVBAgMAnNzMQswCQYDVQQHDAJzczELMAkG
A1UECgwCc3MxCzAJBgNVBAsMAnNzMQswCQYDVQQDDAJzczEQMA4GCSqGSIb3DQEJ
ARYBczAeFw0yMzAzMDcwMTUxMTBaFw0yNjAzMDYwMTUxMTBaMGAxCzAJBgNVBAYT
AkNOMQswCQYDVQQIDAJzczELMAkGA1UEBwwCc3MxCzAJBgNVBAoMAnNzMQswCQYD
VQQLDAJzczELMAkGA1UEAwwCc3MxEDAOBgkqhkiG9w0BCQEWAXMwggEiMA0GCSqG
SIb3DQEBAQUAA4IBDwAwggEKAoIBAQCgmJ8OOfoDEsL19+rzivx6Fgf2ObZQUZKw
FU8ZvUXvj9jSkp4bvNlDdGoOSb2bMwV61ZDJ+hhuIy+2hFf0B5M5Y8uYfOkmHSuE
Uz07W4jXbF7vPUnADqqYcKDLgNamJnw0UjMyecsRKazRKhen5/HnCUBowgJoCKcC
2BGCRj/XjhtPzAIIFEc9CB3Fn73hjCaHeokPHlDTzLGYiO1dSXxQ5KBc8d6hSVvl
zHcw/Npa5/urPIYkSXTrykIk60cuRI9Sv2YyLanpXjGZirw/bZqw5sYOuAGaKbEg
UkZStNkXPW7LHzEgukNVMPPvqpgLwCgvxS7co04PH1tuJihnon4FAgMBAAGjUzBR
MB0GA1UdDgQWBBQePbIRuJRh8clpJRx1IkrGxC92AzAfBgNVHSMEGDAWgBQePbIR
uJRh8clpJRx1IkrGxC92AzAPBgNVHRMBAf8EBTADAQH/MA0GCSqGSIb3DQEBCwUA
A4IBAQAxXQpLKjqSl1HCMf1QDdwHVs1mjzLGIk9BP3g5XS14waooQc2U+VKHfryC
z6bn++dmU3YwT9njSicN70/4PemSPVLXa7VZRz3ao88L9ZxGPdjtjGnlnPL0icTd
/Ns+sbbSOnVMu2P2flK29eKovNcbChNusNUFxlJzOmgtKwjbpnvbiUhFNCzCGNce
Fvwh3ox/gAUIchfA3S4T+hvsTfSWNH0HATm5kNsHRyJY+JeUdXBEq7xGE1AJ6qi7
+SuOMlev39d238SYJ7gRmIMUNZRDJ4U3/Jj6TGrIIisR6UiJyU0zRjQ7lXW40u1Q
7fg/Du0FJbPsoLWsySMYbtD+0N64
-----END CERTIFICATE-----
"#;
        let key = r#"-----BEGIN PRIVATE KEY-----
MIIEvAIBADANBgkqhkiG9w0BAQEFAASCBKYwggSiAgEAAoIBAQCgmJ8OOfoDEsL1
9+rzivx6Fgf2ObZQUZKwFU8ZvUXvj9jSkp4bvNlDdGoOSb2bMwV61ZDJ+hhuIy+2
hFf0B5M5Y8uYfOkmHSuEUz07W4jXbF7vPUnADqqYcKDLgNamJnw0UjMyecsRKazR
Khen5/HnCUBowgJoCKcC2BGCRj/XjhtPzAIIFEc9CB3Fn73hjCaHeokPHlDTzLGY
iO1dSXxQ5KBc8d6hSVvlzHcw/Npa5/urPIYkSXTrykIk60cuRI9Sv2YyLanpXjGZ
irw/bZqw5sYOuAGaKbEgUkZStNkXPW7LHzEgukNVMPPvqpgLwCgvxS7co04PH1tu
Jihnon4FAgMBAAECggEAEXQ89zQcZMideVOqTLFamt85msxvcN/IwEv0lqDQUMbS
wyINvoiCGXd1zls72PoM2qqK67S7gn7fAOh10a8gFGDw///1a//IGr/cPA8I+pbL
45cG5LGDX4GALFXynki4/4u+hjof9JvRrUL0orpN+3TxM+GAFvP3yNKYZo8BgckD
BBIbs62VmmyPrGFaxgAB1VOn4W2QcP58rGKa3PQ0flhnVoXQcDrGat41K4wtYLK1
qcDiziNdxPtmA1WlOifDKBohNthhLppXdIN0ovGSbUG4gCHaMrVhp1UI3kuxvEVn
rLkIG9NKhAcKtgZuZj/r6XI+YT18sF7yP5qfT87BSQKBgQC2i5ZI5cp4yaT7DCln
LtX9P9uTTTuchFkIW/mE3tmO66D68XQ2PDWzv0RZyGM8RJZPHY2xFwG0LJ6oqp5x
6Ss3Wb0BfHJ/QPhCffgb6abT7KSjzghSRQVjnBcIoZL6c+J0iWf7UhjcM4FWKmki
FgWebX1lVk8/gve6E2LlfCVeKQKBgQDhOAIDEpb2SEB+a+ZznjS+OdjMw3EnI+uu
PKrAIBbUv0FyedW7qGDBHZGhAcu7L6Ch/z8NliNbWByoO3E69GS6YCHbhfiNnQyE
7wbhsp0wK5fIBxPuUTr3+lPMsKLH02BxTFsXC/DUX1BmSK/92NIHYxWqHNrSIGFd
kVN6eC/kfQKBgBGfgj/BZ32nwfS2pNygSepsGs+quiGPKWVEM9+fABPrLZxsaRK/
V1PmGDwuu13bJUO4D7DUDscNM7gG2MsYfqKWWEfncspURGNu8+AF+6QkCXUC9Ay1
OyL1s8eSibUCMQ+dIFvD/kBr/IWMDKBMzfgQi/WXkokIJNBjBL4w8Q6ZAoGAV3u7
BFiHPVlpe/ILzWNp125+8WMFpA+G7+Ju7TxJwhAcqwv6Yu+PzdPfiqw46BgjDGoq
outsBoJed1bHr//Y1LCc1jnfB5s2jriOcsM/3cNBLRjavBrfjg212W/Pe1F3R+tC
AtzHiqcPgvu/KRq80tPBSZf1w+OCDqdxxsPCzr0CgYBli/iVi8IwF3JCd6bIPso1
VvKSxm3a5/TWQqyoLy+MZw++fPTg4iExapMb3Qz79qHDhElRUAmYsXIa4l3Y8oEq
cdF/hosG4PTFwCfIbGaT+5uMHnAZCS4/1k2nfhM+H83wxg/81wdVLAw4i7Rc+qyJ
peIJpwo+Kuf964DexDVglw==
-----END PRIVATE KEY-----
"#;
        let shared_config = SharedConfig::from_app_config(AppConfig::default());
        let (tx, rx) = mpsc::channel(1);
        let service_config = ServiceConfig {
            server_type: ServiceType::Https,
            cert_str: Some(cert.to_string()),
            key_str: Some(key.to_string()),
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
        assert!(result.is_ok(), "Expected Ok, got {:?}", result);
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
        assert_eq!(
            result.unwrap_err(),
            AppError("Certificate (cert_str) is missing for TLS service on port 8082".to_string())
        );
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
        assert_eq!(
            result.unwrap_err(),
            AppError("Private key (key_str) is missing for TLS service on port 8086".to_string())
        );
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
            ..Default::default()
        });

        let init_result = init(shared_config.clone()).await;
        assert!(init_result.is_ok());
        {
            let app_config_guard = shared_config.shared_data.lock().unwrap();
            for (_, port, service_conf) in &services_to_init {
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
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        println!("test_init_function completed.");
    }
}
