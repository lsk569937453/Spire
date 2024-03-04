use crate::configuration_service::app_config_service::GLOBAL_CONFIG_MAPPING;
use crate::vojo::app_error::AppError;
use futures::FutureExt;
use http::HeaderMap;
use std::net::SocketAddr;
use tokio::io;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
pub struct TcpProxy {
    pub port: i32,
    pub mapping_key: String,
    pub channel: mpsc::Receiver<()>,
}
impl TcpProxy {
    pub async fn start_proxy(&mut self) -> Result<(), AppError> {
        let listen_addr = format!("0.0.0.0:{}", self.port.clone());
        let mapping_key_clone = self.mapping_key.clone();
        info!("Listening on: {}", listen_addr);
        let listener = TcpListener::bind(listen_addr)
            .await
            .map_err(|e| AppError(e.to_string()))?;
        let reveiver = &mut self.channel;
        loop {
            let accept_future = listener.accept();
            tokio::select! {
               accept_result=accept_future=>{
                if let Ok((inbound, socket_addr))=accept_result{
                   check(mapping_key_clone.clone(),socket_addr).await?;
                   let transfer = transfer(inbound, mapping_key_clone.clone()).map(|r| {
                        if let Err(e) = r {
                            println!("Failed to transfer,error is {}", e);
                        }
                    });
                    tokio::spawn(transfer);
                }
               },
               _=reveiver.recv()=>{
                info!("close the socket of tcp!");
                return Ok(());
               }
            };
        }
    }
}

async fn transfer(mut inbound: TcpStream, mapping_key: String) -> Result<(), AppError> {
    let proxy_addr = get_route_cluster(mapping_key).await?;
    let mut outbound = TcpStream::connect(proxy_addr)
        .await
        .map_err(|err| AppError(err.to_string()))?;

    let (mut ri, mut wi) = inbound.split();
    let (mut ro, mut wo) = outbound.split();
    let client_to_server = async {
        io::copy(&mut ri, &mut wo).await?;
        wo.shutdown().await
    };

    let server_to_client = async {
        io::copy(&mut ro, &mut wi).await?;
        wi.shutdown().await
    };

    let result = tokio::try_join!(client_to_server, server_to_client);

    if result.is_err() {
        error!("Copy stream error!");
    }

    Ok(())
}
async fn check(mapping_key: String, remote_addr: SocketAddr) -> Result<bool, AppError> {
    let value = GLOBAL_CONFIG_MAPPING
        .get(&mapping_key)
        .ok_or("Can not get apiservice from global_mapping")
        .map_err(|err| AppError(err.to_string()))?;
    let service_config = &value.service_config.routes.clone();
    let service_config_clone = service_config.clone();
    if service_config_clone.is_empty() {
        return Err(AppError(String::from("The len of routes is 0")));
    }
    let route = service_config_clone.first().unwrap();
    let is_allowed = route
        .clone()
        .is_allowed(remote_addr.ip().to_string(), None)
        .await?;
    Ok(is_allowed)
}
async fn get_route_cluster(mapping_key: String) -> Result<String, AppError> {
    let value = GLOBAL_CONFIG_MAPPING
        .get(&mapping_key)
        .ok_or("Can not get apiservice from global_mapping")
        .map_err(|err| AppError(err.to_string()))?;
    let service_config = &value.service_config.routes.clone();
    let service_config_clone = service_config.clone();
    if service_config_clone.is_empty() {
        return Err(AppError(String::from("The len of routes is 0")));
    }
    let mut route = service_config_clone.first().unwrap().route_cluster.clone();
    route.get_route(HeaderMap::new()).await.map(|s| s.endpoint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::configuration_service::app_config_service::GLOBAL_APP_CONFIG;
    use crate::utils::uuid::get_uuid;
    use crate::vojo::allow_deny_ip::AllowDenyObject;
    use crate::vojo::allow_deny_ip::AllowType;
    use crate::vojo::api_service_manager::ApiServiceManager;
    use crate::vojo::app_config::ApiService;
    use crate::vojo::app_config::LivenessStatus;
    use crate::vojo::app_config::Matcher;
    use crate::vojo::app_config::{Route, ServiceConfig};
    use crate::vojo::route::AnomalyDetectionStatus;
    use crate::vojo::route::{BaseRoute, LoadbalancerStrategy, RandomBaseRoute, RandomRoute};
    use lazy_static::lazy_static;
    use std::net::TcpListener;
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::Arc;
    use std::{thread, time, vec};
    use tokio::sync::RwLock;

    use tokio::runtime::{Builder, Runtime};

    lazy_static! {
        pub static ref TOKIO_RUNTIME: Runtime = Builder::new_multi_thread()
            .worker_threads(4)
            .thread_name("my-custom-name")
            .thread_stack_size(3 * 1024 * 1024)
            .max_blocking_threads(1000)
            .enable_all()
            .build()
            .unwrap();
    }
    #[test]
    fn test_start_proxy_ok() {
        TOKIO_RUNTIME.spawn(async {
            let (_, receiver) = tokio::sync::mpsc::channel(10);

            let mut tcp_proxy = TcpProxy {
                port: 3352,
                channel: receiver,
                mapping_key: String::from("random key"),
            };
            let _result = tcp_proxy.start_proxy().await;
        });
        TOKIO_RUNTIME.spawn(async {
            let listener = TcpListener::bind("127.0.0.1:3352");
            assert!(listener.is_err());
        });
        let sleep_time = time::Duration::from_millis(200);
        thread::sleep(sleep_time);
    }
    #[test]
    fn test_transfer_error() {
        TOKIO_RUNTIME.spawn(async {
            let tcp_stream = TcpStream::connect("httpbin.org:80").await.unwrap();
            let result = transfer(tcp_stream, String::from("test")).await;
            assert!(result.is_err());
        });
        let sleep_time = time::Duration::from_millis(2000);
        thread::sleep(sleep_time);
    }
    #[test]
    fn test_transfer_ok() {
        let route = LoadbalancerStrategy::Random(RandomRoute {
            routes: vec![RandomBaseRoute {
                base_route: BaseRoute {
                    endpoint: String::from("httpbin.org:80"),
                    try_file: None,
                    is_alive: Arc::new(RwLock::new(None)),
                    anomaly_detection_status: Arc::new(RwLock::new(AnomalyDetectionStatus {
                        consecutive_5xx: 100,
                    })),
                },
            }],
        });
        TOKIO_RUNTIME.spawn(async {
            let (sender, _) = tokio::sync::mpsc::channel(10);

            let api_service_manager = ApiServiceManager {
                sender,
                service_config: ServiceConfig {
                    key_str: None,
                    server_type: crate::vojo::app_config::ServiceType::Tcp,
                    cert_str: None,
                    routes: vec![Route {
                        host_name: None,
                        route_id: get_uuid(),
                        matcher: Default::default(),
                        route_cluster: route,
                        allow_deny_list: None,
                        authentication: None,
                        ratelimit: None,
                        health_check: None,
                        anomaly_detection: None,
                        rewrite_headers: None,

                        liveness_config: None,
                        liveness_status: Arc::new(RwLock::new(LivenessStatus {
                            current_liveness_count: 0,
                        })),
                    }],
                },
            };
            GLOBAL_CONFIG_MAPPING.insert(String::from("test123"), api_service_manager);
            let tcp_stream = TcpStream::connect("httpbin.org:80").await.unwrap();
            let result = transfer(tcp_stream, String::from("test123")).await;
            assert!(result.is_ok());
        });
        let sleep_time = time::Duration::from_millis(2000);
        thread::sleep(sleep_time);
    }
    #[tokio::test]
    async fn test_get_route_cluster_error() {
        let result = get_route_cluster(String::from("testxxxx")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_check_deny_all() {
        let route = LoadbalancerStrategy::Random(RandomRoute {
            routes: vec![RandomBaseRoute {
                base_route: BaseRoute {
                    endpoint: String::from("httpbin.org:80"),
                    try_file: None,
                    is_alive: Arc::new(RwLock::new(None)),
                    anomaly_detection_status: Arc::new(RwLock::new(AnomalyDetectionStatus {
                        consecutive_5xx: 100,
                    })),
                },
            }],
        });
        let (sender, _) = tokio::sync::mpsc::channel(10);

        let api_service_manager = ApiServiceManager {
            sender,
            service_config: ServiceConfig {
                key_str: None,
                server_type: crate::vojo::app_config::ServiceType::Tcp,
                cert_str: None,
                routes: vec![Route {
                    host_name: None,
                    route_id: get_uuid(),
                    matcher: Some(Matcher {
                        prefix: String::from("/"),
                        prefix_rewrite: String::from("test"),
                    }),
                    route_cluster: route,
                    allow_deny_list: Some(vec![AllowDenyObject {
                        limit_type: AllowType::DenyAll,
                        value: None,
                    }]),
                    authentication: None,
                    ratelimit: None,
                    health_check: None,
                    rewrite_headers: None,

                    anomaly_detection: None,
                    liveness_status: Arc::new(RwLock::new(LivenessStatus {
                        current_liveness_count: 0,
                    })),
                    liveness_config: None,
                }],
            },
        };
        let mut write = GLOBAL_APP_CONFIG.write().await;
        write.api_service_config.push(ApiService {
            api_service_id: get_uuid(),
            listen_port: 3478,
            service_config: api_service_manager.service_config.clone(),
        });
        GLOBAL_CONFIG_MAPPING.insert(String::from("3478-TCP"), api_service_manager);
        let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
        let res = check(String::from("3478-TCP"), socket).await;
        assert!(res.is_ok());
        assert!(!res.unwrap());
    }
    #[tokio::test]
    async fn test_check_deny_ip() {
        let route = LoadbalancerStrategy::Random(RandomRoute {
            routes: vec![RandomBaseRoute {
                base_route: BaseRoute {
                    endpoint: String::from("httpbin.org:80"),
                    try_file: None,
                    is_alive: Arc::new(RwLock::new(None)),
                    anomaly_detection_status: Arc::new(RwLock::new(AnomalyDetectionStatus {
                        consecutive_5xx: 100,
                    })),
                },
            }],
        });
        let (sender, _) = tokio::sync::mpsc::channel(10);

        let api_service_manager = ApiServiceManager {
            sender,
            service_config: ServiceConfig {
                key_str: None,
                server_type: crate::vojo::app_config::ServiceType::Tcp,
                cert_str: None,
                routes: vec![Route {
                    host_name: None,
                    route_id: get_uuid(),
                    matcher: Some(Matcher {
                        prefix: String::from("/"),
                        prefix_rewrite: String::from("test"),
                    }),
                    route_cluster: route,
                    allow_deny_list: Some(vec![AllowDenyObject {
                        limit_type: AllowType::Deny,
                        value: Some(String::from("127.0.0.1")),
                    }]),
                    authentication: None,
                    health_check: None,
                    ratelimit: None,
                    anomaly_detection: None,
                    rewrite_headers: None,
                    liveness_config: None,
                    liveness_status: Arc::new(RwLock::new(LivenessStatus {
                        current_liveness_count: 0,
                    })),
                }],
            },
        };
        let mut write = GLOBAL_APP_CONFIG.write().await;
        write.api_service_config.push(ApiService {
            api_service_id: get_uuid(),
            listen_port: 3479,
            service_config: api_service_manager.service_config.clone(),
        });
        GLOBAL_CONFIG_MAPPING.insert(String::from("3479-TCP"), api_service_manager);
        let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
        let res = check(String::from("3479-TCP"), socket).await;
        assert!(res.is_ok());
        assert!(!res.unwrap());
    }
}
