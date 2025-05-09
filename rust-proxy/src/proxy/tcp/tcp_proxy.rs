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
        let listener = TcpListener::bind(listen_addr)
            .await
            .map_err(|e| AppError(e.to_string()))?;
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
async fn check(
    port: i32,
    shared_config: SharedConfig,
    mapping_key: String,
    remote_addr: SocketAddr,
) -> Result<bool, AppError> {
    let app_config = shared_config.shared_data.lock().unwrap().clone();
    let api_service = &app_config
        .api_service_config
        .get(&port)
        .ok_or(AppError(format!(
            "Can not get apiservice from port {}",
            port
        )))?;

    let service_config_clone = api_service.service_config.clone();

    let route = service_config_clone.routes.first().unwrap();
    let is_allowed = route
        .clone()
        .is_allowed(remote_addr.ip().to_string(), None)
        ?;
    Ok(is_allowed)
}
async fn get_route_cluster(
    mapping_key: String,
    shared_config: SharedConfig,
    port: i32,
) -> Result<String, AppError> {
    let app_config = shared_config.shared_data.lock().unwrap().clone();
    let value = app_config
        .api_service_config
        .get(&port)
        .ok_or(AppError(format!(
            "Can not get apiservice from mapping_key {}",
            mapping_key
        )))?;
    let service_config = &value.service_config.routes.clone();
    let service_config_clone = service_config.clone();
    if service_config_clone.is_empty() {
        return Err(AppError(String::from("The len of routes is 0")));
    }
    let mut route = service_config_clone.first().unwrap().route_cluster.clone();
    route.get_route(HeaderMap::new()).map(|s| s.endpoint)
}
