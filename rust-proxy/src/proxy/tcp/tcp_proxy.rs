use crate::vojo::app_error::AppError;
use crate::SharedConfig;
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
    pub shared_config: SharedConfig,
}
impl TcpProxy {
    pub async fn start_proxy(&mut self) -> Result<(), AppError> {
        let listen_addr = format!("0.0.0.0:{}", self.port.clone());
        let mapping_key_clone = self.mapping_key.clone();
        info!("Listening on: {}", listen_addr);
        let listener = TcpListener::bind(listen_addr).await?;
        let reveiver = &mut self.channel;
        loop {
            let accept_future = listener.accept();
            let cloned_config = self.shared_config.clone();
            let port = self.port;
            tokio::select! {
               accept_result=accept_future=>{
                if let Ok((inbound, socket_addr))=accept_result{
                   check(port,cloned_config.clone(),mapping_key_clone.clone(),socket_addr).await?;
                   let transfer = transfer(inbound, mapping_key_clone.clone(),cloned_config.clone(),port).map(|r| {
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

async fn transfer(
    mut inbound: TcpStream,
    mapping_key: String,
    shared_config: SharedConfig,
    port: i32,
) -> Result<(), AppError> {
    let proxy_addr = get_route_cluster(mapping_key, shared_config, port).await?;
    let mut outbound = TcpStream::connect(proxy_addr).await?;

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
async fn check(
    port: i32,
    shared_config: SharedConfig,
    _mapping_key: String,
    remote_addr: SocketAddr,
) -> Result<bool, AppError> {
    let app_config = shared_config.shared_data.lock()?.clone();
    let api_service = &app_config
        .api_service_config
        .get(&port)
        .ok_or(AppError(format!(
            "Can not get apiservice from port {}",
            port
        )))?;

    let service_config_clone = api_service;

    let route = service_config_clone.routes.first().unwrap();
    let is_allowed = route
        .clone()
        .is_allowed(remote_addr.ip().to_string(), None)?;
    Ok(is_allowed)
}
async fn get_route_cluster(
    mapping_key: String,
    shared_config: SharedConfig,
    port: i32,
) -> Result<String, AppError> {
    let app_config = shared_config.shared_data.lock()?.clone();
    let value = app_config
        .api_service_config
        .get(&port)
        .ok_or(AppError(format!(
            "Can not get apiservice from mapping_key {}",
            mapping_key
        )))?;
    let service_config = &value.route_configs.clone();
    let service_config_clone = service_config.clone();
    if service_config_clone.is_empty() {
        return Err(AppError::from("The len of routes is 0"));
    }
    let mut route = service_config_clone
        .first()
        .ok_or("service_config_clone is empty")?
        .router
        .clone();
    route.get_route(&HeaderMap::new()).map(|s| s.get_endpoint())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::vojo::app_config::{ApiService, AppConfig, RouteConfig};

    use crate::vojo::router::WeightBasedRoute;
    use crate::vojo::router::WeightedRouteItem;
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr};

    use std::sync::{Arc, Mutex};

    fn create_mock_shared_config(
        port: i32,
        _allowed_ips: Vec<&str>,
        _endpoint: &str,
    ) -> SharedConfig {
        let header_based = WeightBasedRoute {
            routes: vec![WeightedRouteItem {
                weight: 1,
                index: 0,
                endpoint: "http://www.baidu.com".to_string(),
                ..Default::default()
            }],
        };
        let route = RouteConfig {
            route_id: "test_route".to_string(),
            router: crate::vojo::router::Router::WeightBased(header_based),
            ..Default::default()
        };
        let api_service = ApiService {
            route_configs: vec![route],
            ..Default::default()
        };

        let mut api_service_config = HashMap::new();
        api_service_config.insert(port, api_service);

        SharedConfig {
            shared_data: Arc::new(Mutex::new(AppConfig {
                api_service_config,
                ..Default::default()
            })),
        }
    }

    #[tokio::test]
    async fn test_check_allowed_ip() {
        let port = 8080;
        let shared_config = create_mock_shared_config(port, vec!["127.0.0.1"], "127.0.0.1:8080");

        let allowed_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 1234);
        let denied_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), 1234);

        let result = check(
            port,
            shared_config.clone(),
            "test_key".to_string(),
            allowed_addr,
        )
        .await;
        assert!(result.unwrap());

        let result = check(port, shared_config, "test_key".to_string(), denied_addr).await;
        println!("{:?}", result);
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_get_route_cluster_success() {
        let port = 8080;
        let shared_config = create_mock_shared_config(port, vec!["127.0.0.1"], "127.0.0.1:8080");

        let result = get_route_cluster("test_key".to_string(), shared_config, port).await;
        assert_eq!(result.unwrap(), "http://www.baidu.com");
    }

    #[tokio::test]
    async fn test_get_route_cluster_no_service() {
        let shared_config = SharedConfig {
            shared_data: Arc::new(Mutex::new(AppConfig {
                api_service_config: HashMap::new(),
                ..Default::default()
            })),
        };

        let result = get_route_cluster("test_key".to_string(), shared_config, 8080).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_proxy_shutdown() {
        let (tx, rx) = mpsc::channel(1);
        let mut proxy = TcpProxy {
            port: 7070,
            mapping_key: "test".to_string(),
            channel: rx,
            shared_config: create_mock_shared_config(7070, vec!["127.0.0.1"], "127.0.0.1:7070"),
        };

        let _ = tx.send(()).await;

        let result = proxy.start_proxy().await;
        assert!(result.is_ok());
    }
    let mut route = service_config_clone.first().unwrap().route_cluster.clone();
    route.get_route(HeaderMap::new()).map(|s| s.endpoint)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::vojo::app_config::{ApiService, AppConfig, Route, ServiceConfig};
    use crate::vojo::route::BaseRoute;
    use crate::vojo::route::LoadbalancerStrategy;

    use crate::vojo::route::WeightBasedRoute;
    use crate::vojo::route::WeightRoute;

    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr};

    use std::sync::{Arc, Mutex};

    fn create_mock_shared_config(
        port: i32,
        allowed_ips: Vec<&str>,
        endpoint: &str,
    ) -> SharedConfig {
        let header_based = WeightBasedRoute {
            routes: vec![WeightRoute {
                weight: 1,
                index: 0,
                base_route: BaseRoute {
                    endpoint: "http://www.baidu.com".to_string(),
                    ..Default::default()
                },
            }],
        };
        let route = Route {
            route_id: "test_route".to_string(),
            route_cluster: LoadbalancerStrategy::WeightBased(header_based),
            ..Default::default()
        };
        let api_service = ApiService {
            service_config: ServiceConfig {
                routes: vec![route],
                ..Default::default()
            },
            ..Default::default()
        };

        let mut api_service_config = HashMap::new();
        api_service_config.insert(port, api_service);

        SharedConfig {
            shared_data: Arc::new(Mutex::new(AppConfig {
                api_service_config,
                ..Default::default()
            })),
        }
    }

    #[tokio::test]
    async fn test_check_allowed_ip() {
        let port = 8080;
        let shared_config = create_mock_shared_config(port, vec!["127.0.0.1"], "127.0.0.1:8080");

        let allowed_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 1234);
        let denied_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), 1234);

        // 测试允许的IP
        let result = check(
            port,
            shared_config.clone(),
            "test_key".to_string(),
            allowed_addr,
        )
        .await;
        assert!(result.unwrap());

        // 测试禁止的IP
        let result = check(port, shared_config, "test_key".to_string(), denied_addr).await;
        println!("{:?}", result);
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_get_route_cluster_success() {
        let port = 8080;
        let shared_config = create_mock_shared_config(port, vec!["127.0.0.1"], "127.0.0.1:8080");

        let result = get_route_cluster("test_key".to_string(), shared_config, port).await;
        assert_eq!(result.unwrap(), "http://www.baidu.com");
    }

    #[tokio::test]
    async fn test_get_route_cluster_no_service() {
        let shared_config = SharedConfig {
            shared_data: Arc::new(Mutex::new(AppConfig {
                api_service_config: HashMap::new(),
                ..Default::default()
            })),
        };

        let result = get_route_cluster("test_key".to_string(), shared_config, 8080).await;
        assert!(result.is_err());
    }

    // #[tokio::test]
    // async fn test_transfer_basic() {
    //     let (mut client, server) = tokio::io::duplex(1024);
    //     let mock_config = create_mock_shared_config(8080, vec![], "127.0.0.1:8080");

    //     // 启动传输任务
    //     let transfer_task = tokio::spawn(async move {
    //         transfer(server, "test_key".to_string(), mock_config, 8080).await
    //     });

    //     // 客户端写入数据
    //     client.write_all(b"ping").await.unwrap();
    //     client.shutdown().await.unwrap();

    //     // 检查服务端接收的数据
    //     let mut buf = [0u8; 4];
    //     transfer_task.await.unwrap().unwrap();
    //     client.read_exact(&mut buf).await.unwrap();
    //     assert_eq!(&buf, b"ping");
    // }

    #[tokio::test]
    async fn test_proxy_shutdown() {
        let (tx, rx) = mpsc::channel(1);
        let mut proxy = TcpProxy {
            port: 8080,
            mapping_key: "test".to_string(),
            channel: rx,
            shared_config: create_mock_shared_config(8080, vec!["127.0.0.1"], "127.0.0.1:8080"),
        };

        // 发送关闭信号
        let _ = tx.send(()).await;

        // 应该正常退出
        let result = proxy.start_proxy().await;
        assert!(result.is_ok());
    }
}
