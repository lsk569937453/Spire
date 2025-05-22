use crate::constants::common_constants::GRPC_STATUS_HEADER;
use crate::constants::common_constants::GRPC_STATUS_OK;
use crate::proxy::proxy_trait::ChainTrait;
use crate::proxy::proxy_trait::CommonCheckRequest;
use crate::proxy::proxy_trait::SpireContext;
use crate::vojo::app_error::AppError;
use h2::client;
use h2::server;
use h2::server::SendResponse;
use h2::RecvStream;
use h2::SendStream;
use http::version::Version;
use http::Response;
use http::{Method, Request};
use hyper::body::Bytes;

use crate::SharedConfig;
use rustls_pki_types::CertificateDer;
use std::io::BufReader;
use std::net::SocketAddr;
use std::net::ToSocketAddrs;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_rustls::server::TlsStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::{rustls, TlsAcceptor};
use url::Url;

pub struct GrpcProxy {
    pub port: i32,
    pub channel: mpsc::Receiver<()>,
    pub mapping_key: String,
    pub shared_config: SharedConfig,
}
pub async fn start_task(
    port: i32,
    shared_config: SharedConfig,
    tcp_stream: TcpStream,
    mapping_key: String,
    peer_addr: SocketAddr,
) -> Result<(), AppError> {
    let mut connection = server::handshake(tcp_stream)
        .await
        .map_err(|e| AppError(e.to_string()))?;
    while let Some(request_result) = connection.accept().await {
        if let Ok((request, respond)) = request_result {
            let mapping_key_cloned = mapping_key.clone();
            let cloned_config = shared_config.clone();
            tokio::spawn(async move {
                let result = request_outbound_adapter(
                    port,
                    cloned_config,
                    request,
                    respond,
                    mapping_key_cloned,
                    peer_addr,
                )
                .await;
                if let Err(err) = result {
                    error!("Grpc request outbound error,the error is {}", err);
                }
            });
        }
    }
    Ok(())
}
pub async fn start_tls_task(
    port: i32,
    shared_config: SharedConfig,
    tcp_stream: TlsStream<TcpStream>,
    mapping_key: String,
    peer_addr: SocketAddr,
) -> Result<(), AppError> {
    let mut connection = server::handshake(tcp_stream)
        .await
        .map_err(|e| AppError(e.to_string()))?;
    while let Some(request_result) = connection.accept().await {
        if let Ok((request, respond)) = request_result {
            let mapping_key_cloned = mapping_key.clone();
            let cloned_config = shared_config.clone();

            tokio::spawn(async move {
                let result = request_outbound_adapter(
                    port,
                    cloned_config,
                    request,
                    respond,
                    mapping_key_cloned,
                    peer_addr,
                )
                .await;
                if let Err(err) = result {
                    error!("Grpc request outbound error,the error is {}", err);
                }
            });
        }
    }
    Ok(())
}
async fn request_outbound_adapter(
    port: i32,
    shared_config: SharedConfig,
    inbount_request: Request<RecvStream>,
    inbound_respond: SendResponse<Bytes>,
    mapping_key: String,
    peer_addr: SocketAddr,
) -> Result<(), AppError> {
    request_outbound(
        port,
        shared_config,
        inbount_request,
        inbound_respond,
        mapping_key,
        peer_addr,
        CommonCheckRequest {},
    )
    .await
}
impl GrpcProxy {
    pub async fn start_proxy(&mut self) -> Result<(), AppError> {
        let port_clone = self.port;
        let addr = SocketAddr::from(([0, 0, 0, 0], port_clone as u16));
        info!("Listening on grpc://{}", addr);
        let listener = TcpListener::bind(addr).await?;
        let mapping_key = self.mapping_key.clone();
        let reveiver = &mut self.channel;

        loop {
            let accept_future = listener.accept();
            tokio::select! {
               accept_result=accept_future=>{
                let cloned_port=self.port;
                let cloned_config=self.shared_config.clone();
                if let Ok((socket, peer_addr))=accept_result{
                    tokio::spawn(start_task(cloned_port,cloned_config,socket, mapping_key.clone(), peer_addr));
                }
               },
               _=reveiver.recv()=>{
                info!("close the socket of grpc!");
                return Ok(());
               }
            };
        }
    }
    pub async fn start_tls_proxy(
        &mut self,
        pem_str: String,
        key_str: String,
    ) -> Result<(), AppError> {
        let port_clone = self.port;
        let addr = SocketAddr::from(([0, 0, 0, 0], port_clone as u16));
        let mut cer_reader = BufReader::new(pem_str.as_bytes());
        // let certs = rustls_pemfile::certs(&mut cer_reader)
        //     .unwrap()
        //     .iter()
        //     .map(|s| rustls::Certificate((*s).clone()))
        //     .collect();
        let certs: Vec<CertificateDer<'_>> = rustls_pemfile::certs(&mut cer_reader)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AppError(e.to_string()))?;

        let mut key_reader = BufReader::new(key_str.as_bytes());
        let key_der = rustls_pemfile::private_key(&mut key_reader)
            .map_err(|e| AppError(e.to_string()))?
            .ok_or("key_der is none")?;

        let tls_cfg = {
            let cfg = rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(certs, key_der)?;
            Arc::new(cfg)
        };
        let tls_acceptor = TlsAcceptor::from(tls_cfg);

        info!("Listening on grpc with tls://{}", addr);
        let listener = TcpListener::bind(addr).await?;
        let mapping_key = self.mapping_key.clone();
        let reveiver = &mut self.channel;

        loop {
            let accept_future = listener.accept();
            tokio::select! {
               accept_result=accept_future=>{
                let cloned_port=self.port;
                let cloned_config=self.shared_config.clone();
                if let Ok((tcp_stream, peer_addr))=accept_result{
                    if let Ok(tls_streams) = tls_acceptor.accept(tcp_stream).await {
                        tokio::spawn(start_tls_task(cloned_port,cloned_config,tls_streams, mapping_key.clone(), peer_addr));
                    }
                }
               },
               _=reveiver.recv()=>{
                info!("close the socket!");
                return Ok(());
               }
            };
        }
    }
}

async fn copy_io(
    mut send_stream: SendStream<Bytes>,
    mut recv_stream: RecvStream,
) -> Result<(), AppError> {
    let mut flow_control = recv_stream.flow_control().clone();
    while let Some(chunk_result) = recv_stream.data().await {
        let chunk_bytes = chunk_result.map_err(|e| AppError(e.to_string()))?;
        debug!("Data from outbound: {:?}", chunk_bytes.clone());
        send_stream
            .send_data(chunk_bytes.clone(), false)
            .map_err(|e| AppError(e.to_string()))?;
        flow_control
            .release_capacity(chunk_bytes.len())
            .map_err(|e| AppError(e.to_string()))?;
    }
    if let Ok(Some(header)) = recv_stream.trailers().await {
        send_stream
            .send_trailers(header)
            .map_err(|e| AppError(e.to_string()))?;
    }
    Ok(())
}
async fn request_outbound(
    port: i32,
    shared_config: SharedConfig,
    inbount_request: Request<RecvStream>,
    mut inbound_respond: SendResponse<Bytes>,
    mapping_key: String,
    peer_addr: SocketAddr,
    check_trait: impl ChainTrait,
) -> Result<(), AppError> {
    debug!("{:?}", inbount_request);
    let (inbound_parts, inbound_body) = inbount_request.into_parts();

    let inbound_headers = inbound_parts.headers.clone();
    let uri = inbound_parts.uri.clone();
    let mut spire_context = SpireContext::new(port, None);
    let check_result = check_trait
        .handle_before_request(
            shared_config,
            port,
            mapping_key.clone(),
            inbound_headers,
            uri,
            peer_addr,
            &mut spire_context,
        )
        .await?;
    if check_result.is_none() {
        return Err(AppError(String::from(
            "The request has been denied by the proxy!",
        )));
    }
    let request_path = check_result.ok_or("check_result is none")?.request_path;
    let url = Url::parse(&request_path).map_err(|e| AppError(e.to_string()))?;
    let cloned_url = url.clone();
    let host = cloned_url
        .host()
        .ok_or(AppError(String::from("Parse host error!")))?;
    let port = cloned_url
        .port()
        .ok_or(AppError(String::from("Parse host error!")))?;
    debug!("The host is {}", host);

    let addr = format!("{}:{}", host, port)
        .to_socket_addrs()
        .map_err(|e| AppError(e.to_string()))?
        .next()
        .ok_or(AppError(String::from("Parse the domain error!")))?;
    debug!("The addr is {}", addr);
    let host_str = host.to_string();

    let send_request_poll = if request_path.clone().contains("https") {
        let mut root_cert_store = rustls::RootCertStore::empty();
        root_cert_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

        let mut config = rustls::ClientConfig::builder()
            .with_root_certificates(root_cert_store)
            .with_no_client_auth();
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        let tls_connector = TlsConnector::from(Arc::new(config));
        let stream = TcpStream::connect(&addr)
            .await
            .map_err(|e| AppError(e.to_string()))?;
        let domain = rustls_pki_types::ServerName::try_from(host_str.as_str())
            .map_err(|e| AppError(e.to_string()))?
            .to_owned();
        debug!("The domain name is {}", host);
        let stream = tls_connector
            .connect(domain, stream)
            .await
            .map_err(|e| AppError(e.to_string()))?;
        let (send_request, connection) = client::handshake(stream)
            .await
            .map_err(|e| AppError(e.to_string()))?;
        tokio::spawn(async move {
            let connection_result = connection.await;
            if let Err(err) = connection_result {
                error!("Cause error in grpc https connection,the error is {}.", err);
            } else {
                debug!("The connection has closed!");
            }
        });
        send_request
    } else {
        let tcpstream = TcpStream::connect(addr)
            .await
            .map_err(|e| AppError(e.to_string()))?;
        let (send_request, connection) = client::handshake(tcpstream)
            .await
            .map_err(|e| AppError(e.to_string()))?;
        tokio::spawn(async move {
            connection.await.unwrap();
            debug!("The connection has closed!");
        });
        send_request
    };

    debug!("request path is {}", url);
    let mut send_request = send_request_poll
        .ready()
        .await
        .map_err(|e| AppError(e.to_string()))?;
    let request = Request::builder()
        .method(Method::POST)
        .version(Version::HTTP_2)
        .uri(url.to_string())
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .body(())?;
    debug!("Our bound request is {:?}", request);
    let (response, outbound_send_stream) = send_request
        .send_request(request, false)
        .map_err(|e| AppError(e.to_string()))?;
    tokio::spawn(async {
        if let Err(err) = copy_io(outbound_send_stream, inbound_body).await {
            error!("Copy from inbound to outboud error,the error is {}", err);
        }
    });

    let (head, outboud_response_body) = response
        .await
        .map_err(|e| AppError(e.to_string()))?
        .into_parts();

    debug!("Received response: {:?}", head);

    let header_map = head.headers.clone();
    let is_grpc_status_ok = header_map
        .get(GRPC_STATUS_HEADER)
        .map(|item| item.to_str().unwrap_or_default() != GRPC_STATUS_OK)
        .unwrap_or(false);
    let inbound_response = Response::from_parts(head, ());

    let send_stream = inbound_respond
        .send_response(inbound_response, is_grpc_status_ok)
        .map_err(|e| AppError(e.to_string()))?;

    tokio::spawn(async {
        if let Err(err) = copy_io(send_stream, outboud_response_body).await {
            error!("Copy from outbound to inbound error,the error is {}", err);
        }
    });
    Ok(())
}
