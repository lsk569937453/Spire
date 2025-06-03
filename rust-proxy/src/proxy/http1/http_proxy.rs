use crate::constants::common_constants;
use crate::constants::common_constants::DEFAULT_HTTP_TIMEOUT;
use crate::monitor::prometheus_exporter::{get_timer_list, inc};
use crate::proxy::http1::http_client::HttpClients;

use crate::vojo::app_error::AppError;
use crate::vojo::cli::SharedConfig;
use bytes::Bytes;
use http::{HeaderValue, Uri};
use hyper::body::Incoming;
use hyper::header;
use hyper::header::{CONNECTION, SEC_WEBSOCKET_KEY};
use hyper::Method;
use hyper::StatusCode;

use crate::proxy::http1::websocket_proxy::server_upgrade;
use crate::proxy::proxy_trait::{ChainTrait, SpireContext};
use crate::proxy::proxy_trait::{CommonCheckRequest, RouterDestination};
use http::uri::PathAndQuery;
use http_body_util::{combinators::BoxBody, BodyExt, Full};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_staticfile::Static;
use hyper_util::rt::TokioIo;
use prometheus::HistogramTimer;
use rustls_pki_types::CertificateDer;
use serde_json::json;
use std::io::BufReader;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;
pub struct HttpProxy {
    pub port: i32,
    pub channel: mpsc::Receiver<()>,
    pub mapping_key: String,
    pub shared_config: SharedConfig,
}

impl HttpProxy {
    pub async fn start_http_server(&mut self) -> Result<(), AppError> {
        let port_clone = self.port;
        let addr = SocketAddr::from(([0, 0, 0, 0], port_clone as u16));
        let client = HttpClients::new();
        let mapping_key_clone1 = self.mapping_key.clone();
        let reveiver = &mut self.channel;

        let listener = TcpListener::bind(addr).await?;
        info!("Listening on http://{}", addr);
        loop {
            tokio::select! {
               Ok((stream,addr))= listener.accept()=>{
                let client_cloned = client.clone();
                let cloned_shared_config=self.shared_config.clone();
                let cloned_port=self.port;
                let mapping_key2 = mapping_key_clone1.clone();
                tokio::spawn(async move {
                    let io = TokioIo::new(stream);

                    if let Err(err) = http1::Builder::new()
                    .preserve_header_case(true)
                    .title_case_headers(true)
                        .serve_connection(
                            io,
                            service_fn(move |req: Request<Incoming>| {
                                let req = req.map(|item| {
                                    item.map_err(AppError::from).boxed()
                                });
                                proxy_adapter(cloned_port,cloned_shared_config.clone(),client_cloned.clone(), req, mapping_key2.clone(), addr)
                            }),
                        )
                        .await
                    {
                        error!("Error serving connection: {:?}", err);
                    }
                });
                },
                _ = reveiver.recv() => {
                    info!("http server stoped");
                    break;
                }
            }
        }

        Ok(())
    }
    pub async fn start_https_server(
        &mut self,
        pem_str: String,
        key_str: String,
    ) -> Result<(), AppError> {
        let port_clone = self.port;
        let addr = SocketAddr::from(([0, 0, 0, 0], port_clone as u16));
        let client = HttpClients::new();
        let mapping_key_clone1 = self.mapping_key.clone();

        let mut cer_reader = BufReader::new(pem_str.as_bytes());
        let certs: Vec<CertificateDer<'_>> =
            rustls_pemfile::certs(&mut cer_reader).collect::<Result<Vec<_>, _>>()?;

        let mut key_reader = BufReader::new(key_str.as_bytes());
        let key_der = rustls_pemfile::private_key(&mut key_reader)?
            .ok_or_else(|| AppError("Key not found in PEM file".to_string()))?;

        let tls_cfg = {
            let cfg = rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(certs, key_der)?;
            Arc::new(cfg)
        };
        let tls_acceptor = TlsAcceptor::from(tls_cfg);
        let reveiver = &mut self.channel;

        let listener = TcpListener::bind(addr).await?;
        info!("Listening on http://{}", addr);
        loop {
            tokio::select! {
                    Ok((tcp_stream,addr))= listener.accept()=>{
                let tls_acceptor = tls_acceptor.clone();
                let cloned_shared_config=self.shared_config.clone();
                let cloned_port=self.port;
                let client = client.clone();
                let mapping_key2 = mapping_key_clone1.clone();
                tokio::spawn(async move {
                    let tls_stream = match tls_acceptor.accept(tcp_stream).await {
                        Ok(tls_stream) => tls_stream,
                        Err(err) => {
                            error!("failed to perform tls handshake: {err:#}");
                            return;
                        }
                    };
                    let io = TokioIo::new(tls_stream);
                    let service = service_fn(move |req: Request<Incoming>| {
                        let req = req
                            .map(|item| item.map_err(AppError::from).boxed());

                        proxy_adapter(cloned_port,cloned_shared_config.clone(),client.clone(), req, mapping_key2.clone(), addr)
                    });
                    if let Err(err) = http1::Builder::new().serve_connection(io, service).await {
                        error!("Error serving connection: {:?}", err);
                    }
                });
            },
                    _ = reveiver.recv() => {
                        info!("https server stoped");
                        break;
                    }
                }
        }

        Ok(())
    }
}
async fn proxy_adapter(
    port: i32,
    shared_config: SharedConfig,
    client: HttpClients,
    req: Request<BoxBody<Bytes, AppError>>,
    mapping_key: String,
    remote_addr: SocketAddr,
) -> Result<Response<BoxBody<Bytes, Infallible>>, AppError> {
    let result =
        proxy_adapter_with_error(port, shared_config, client, req, mapping_key, remote_addr).await;
    match result {
        Ok(res) => Ok(res),
        Err(err) => {
            error!("The error is {}.", err);
            let json_value = json!({
                "error": err.to_string(),
            });
            Ok(Response::builder().status(StatusCode::NOT_FOUND).body(
                Full::new(Bytes::copy_from_slice(json_value.to_string().as_bytes())).boxed(),
            )?)
        }
    }
}
async fn proxy_adapter_with_error(
    port: i32,
    shared_config: SharedConfig,
    client: HttpClients,
    req: Request<BoxBody<Bytes, AppError>>,
    mapping_key: String,
    remote_addr: SocketAddr,
) -> Result<Response<BoxBody<Bytes, AppError>>, AppError> {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let path = uri
        .path_and_query()
        .unwrap_or(&PathAndQuery::from_static("/hello?world"))
        .to_string();
    let current_time = SystemTime::now();
    let monitor_timer_list = get_timer_list(mapping_key.clone(), path.clone())
        .iter()
        .map(|item| item.start_timer())
        .collect::<Vec<HistogramTimer>>();
    let res = proxy(
        port,
        shared_config,
        client,
        req,
        mapping_key.clone(),
        remote_addr,
        CommonCheckRequest {},
    )
    .await
    .unwrap_or_else(|err| {
        error!("The error is {}.", err);
        let json_value = json!({
            "response_code": -1,
            "response_object": format!("{}", err)
        });
        Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(
                Full::new(Bytes::copy_from_slice(json_value.to_string().as_bytes()))
                    .map_err(AppError::from)
                    .boxed(),
            )
            .unwrap()
    });
    let elapsed_time_res = current_time.elapsed()?;

    let status = res.status().as_u16();
    monitor_timer_list
        .into_iter()
        .for_each(|item| item.observe_duration());
    inc(mapping_key.clone(), path.clone(), status);

    info!(
        "{} - -  \"{} {} HTTP/1.1\" {}  \"-\" \"-\"  {:?}",
        remote_addr, method, path, status, elapsed_time_res
    );
    Ok(res)
}

async fn proxy(
    port: i32,
    shared_config: SharedConfig,
    client: HttpClients,
    mut req: Request<BoxBody<Bytes, AppError>>,
    mapping_key: String,
    remote_addr: SocketAddr,
    chain_trait: impl ChainTrait,
) -> Result<Response<BoxBody<Bytes, Infallible>>, AppError> {
    debug!("req: {:?}", req);

    let inbound_headers = req.headers();
    let uri = req.uri().clone();
    let mut spire_context = SpireContext::new(port, None);
    let handling_result = chain_trait
        .get_destination(
            shared_config.clone(),
            port,
            mapping_key.clone(),
            inbound_headers,
            uri,
            remote_addr,
            &mut spire_context,
        )
        .await?;
    debug!("The get_destination is {:?}", handling_result);
    if handling_result.is_none() {
        return Ok(Response::builder()
            .status(StatusCode::FORBIDDEN)
            .body(Full::new(Bytes::from(common_constants::DENY_RESPONSE)).boxed())?);
    }

    if req.method() == Method::OPTIONS
        && req.headers().contains_key(header::ORIGIN)
        && req
            .headers()
            .contains_key(header::ACCESS_CONTROL_REQUEST_METHOD)
    {
        if let Some(cors_config) = spire_context.cors_configed()? {
            return chain_trait.handle_preflight(cors_config, "");
        }
    }
    if inbound_headers.clone().contains_key(CONNECTION)
        && inbound_headers.contains_key(SEC_WEBSOCKET_KEY)
    {
        debug!(
            "The request has been updated to websocket,the req is {:?}!",
            req
        );
        return server_upgrade(req, handling_result, client).await;
    }

    if let Some(check_request) = handling_result {
        let request_path = check_request.request_path.as_str();
        let router_destination = check_request.router_destination;
        if let Some(middlewares) = spire_context.middlewares.clone() {
            if !middlewares.is_empty() {
                chain_trait
                    .handle_before_request(middlewares, remote_addr, &mut req)
                    .await?;
            }
        }
        let mut res = if router_destination.is_file() {
            let mut parts = req.uri().clone().into_parts();
            parts.path_and_query = Some(request_path.try_into()?);
            *req.uri_mut() = Uri::from_parts(parts)?;
            route_file(router_destination, req).await?
        } else {
            *req.uri_mut() = request_path.parse()?;
            let host = req
                .uri()
                .host()
                .ok_or("Uri to host cause error")?
                .to_string();
            req.headers_mut()
                .insert(http::header::HOST, HeaderValue::from_str(&host)?);

            let request_future = if request_path.contains("https") {
                client.request_https(req, DEFAULT_HTTP_TIMEOUT)
            } else {
                client.request_http(req, DEFAULT_HTTP_TIMEOUT)
            };
            let response_result = match request_future.await {
                Ok(response) => response.map_err(AppError::from),
                _ => {
                    return Err(AppError(format!(
                        "Request time out,the uri is {}",
                        request_path
                    )))
                }
            };
            response_result?
                .map(|b| b.boxed())
                .map(|item: BoxBody<Bytes, hyper::Error>| {
                    item.map_err(|_| -> Infallible { unreachable!() }).boxed()
                })
        };
        if let Some(middlewares) = spire_context.middlewares {
            if !middlewares.is_empty() {
                chain_trait
                    .handle_before_response(middlewares, request_path, &mut res)
                    .await?;
            }
        }
        return Ok(res);
    }
    Ok(Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(
            Full::new(Bytes::from(common_constants::NOT_FOUND))
                .map_err(AppError::from)
                .boxed(),
        )
        .unwrap())
}

async fn route_file(
    router_destination: RouterDestination,
    req: Request<BoxBody<Bytes, Infallible>>,
) -> Result<Response<BoxBody<Bytes, Infallible>>, AppError> {
    let static_ = Static::new(Path::new(router_destination.get_endpoint().as_str()));
    static_
        .clone()
        .serve(req)
        .await
        .map(|item| {
            item.map(|body| {
                body.boxed()
                    .map_err(|_| -> AppError { unreachable!() })
                    .boxed()
            })
        })
        .map_err(AppError::from)
}
#[cfg(test)]
mod tests {
    use super::*;

    use crate::vojo::app_config::Matcher;
    use crate::vojo::app_config::{ApiService, RouteConfig, ServiceConfig};
    use crate::vojo::router::{BaseRoute, RandomRoute, Router};
    use crate::{vojo::router::StaticFileRoute, AppConfig};
    use http::HeaderMap;
    use std::collections::HashMap;

    use std::net::IpAddr;
    use std::net::Ipv4Addr;
    use std::sync::Mutex;

    #[test]
    fn test_http_proxy_creation() {
        let (tx, rx) = mpsc::channel(1);
        let shared_config = SharedConfig {
            shared_data: Arc::new(Mutex::new(AppConfig::default())),
        };

        let proxy = HttpProxy {
            port: 8080,
            channel: rx,
            mapping_key: "test".to_string(),
            shared_config,
        };

        assert_eq!(proxy.port, 8080);
        assert_eq!(proxy.mapping_key, "test");
    }

    #[tokio::test]
    async fn test_proxy_adapter_error_handling() {
        let client = HttpClients::new();
        let shared_config = SharedConfig {
            shared_data: Arc::new(Mutex::new(AppConfig::default())),
        };
        let remote_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);

        let req = Request::builder()
            .uri("invalid://uri")
            .body(Full::new(Bytes::from("test")).boxed())
            .unwrap();

        let result = proxy_adapter(
            8080,
            shared_config,
            client,
            req,
            "test".to_string(),
            remote_addr,
        )
        .await;

        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
    #[tokio::test]
    async fn test_options_preflight_request() {
        // let _ = setup_logger_for_test();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://127.0.0.1:8080"),
        );
        headers.insert(
            header::ACCESS_CONTROL_REQUEST_METHOD,
            HeaderValue::from_static("POST"),
        );

        let client = HttpClients::new();
        let shared_config = SharedConfig {
            shared_data: Arc::new(Mutex::new(AppConfig {
                static_config: Default::default(),
                api_service_config: HashMap::from([(
                    8080,
                    ApiService {
                        listen_port: 8080,
                        service_config: ServiceConfig {
                            route_configs: vec![RouteConfig {
                                router: Router::Random(RandomRoute {
                                    routes: vec![BaseRoute {
                                        endpoint: "http://127.0.0.1:9394".to_string(),
                                        ..Default::default()
                                    }],
                                }),
                                matcher: Some(Matcher {
                                    prefix: "/".to_string(),
                                    prefix_rewrite: "/".to_string(),
                                }),

                                ..Default::default()
                            }],

                            ..Default::default()
                        },
                        ..Default::default()
                    },
                )]),
            })),
        };
        let remote_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);

        let mut req = Request::builder()
            .method(Method::OPTIONS)
            .uri("http://127.0.0.1:8080/test")
            .body(Full::new(Bytes::from("")).boxed())
            .unwrap();
        req.headers_mut().extend(headers);

        let result = proxy(
            8080,
            shared_config,
            client,
            req,
            "test".to_string(),
            remote_addr,
            CommonCheckRequest {},
        )
        .await;
        println!("result is {:?}", result);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_route_file() {
        let router_destination = RouterDestination::File(StaticFileRoute {
            doc_root: "./test".to_string(),
        });

        let req = Request::builder()
            .uri("http://localhost/test.txt")
            .body(Full::new(Bytes::from("")).boxed())
            .unwrap();

        let result = route_file(router_destination, req).await;
        assert!(result.is_ok());
    }
}
