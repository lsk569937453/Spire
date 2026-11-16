use crate::control_plane::cert_loader::load_tls_config_multi;
use crate::control_plane::cert_loader::watch_for_certificate_changes;
use crate::middleware::middlewares::MiddleWares;
use crate::middleware::middlewares::RequestSnapshot;
use crate::monitor::prometheus_exporter::metrics;
use crate::proxy::http1::app_clients::AppClients;
use crate::proxy::http1::middleware_phases::PhaseInfo;
use crate::proxy::http1::middleware_phases::apply_log_phases;
use crate::proxy::http1::middleware_phases::apply_request_middlewares;
use crate::proxy::http1::middleware_phases::apply_response_phases;
use crate::proxy::http1::middleware_phases::denial_response;
use crate::proxy::http1::middleware_phases::has_wasm;
use crate::proxy::http1::upstream_forward::UpstreamCall;
use crate::proxy::http1::upstream_forward::forward_grpc_upstream;
use crate::proxy::http1::upstream_forward::forward_http_upstream;
use crate::proxy::http1::websocket_proxy::server_upgrade;
use crate::proxy::proxy_trait::DestinationResult;
use crate::proxy::proxy_trait::HandlingResult;
use crate::proxy::proxy_trait::{ChainTrait, SpireContext};
use crate::proxy::proxy_trait::{CommonCheckRequest, RouterDestination};
use crate::vojo::app_error::AppError;
use crate::vojo::cli::SharedConfig;
use bytes::Bytes;
use http::{HeaderValue, Uri};
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::Method;
use hyper::StatusCode;
use hyper::body::Incoming;
use hyper::header;
use hyper::header::{CONNECTION, SEC_WEBSOCKET_KEY};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_staticfile::Static;
use hyper_util::rt::TokioIo;
use rustls::ServerConfig;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::RwLock;
use std::time::Instant;
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
        let client = AppClients::new(self.shared_config.clone(), self.port).await?;
        let mapping_key_clone1 = self.mapping_key.clone();
        let reveiver = &mut self.channel;

        let listener = TcpListener::bind(addr).await?;
        info!("Listening on http://{addr}");
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
                                proxy_adapter(cloned_port,cloned_shared_config.clone(),client_cloned.clone(), req, mapping_key2.clone(), addr)
                            }),
                        ).with_upgrades()
                        .await
                    {
                        error!("Error serving connection: {err:?}");
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
    pub async fn start_https_server(&mut self, domains: Vec<String>) -> Result<(), AppError> {
        let port_clone = self.port;
        let addr = SocketAddr::from(([0, 0, 0, 0], port_clone as u16));
        let client = AppClients::new(self.shared_config.clone(), self.port).await?;
        let mapping_key_clone1 = self.mapping_key.clone();

        let tls_cfg = load_tls_config_multi(&domains)?;
        let shared_tls_config: Arc<RwLock<ServerConfig>> = Arc::new(RwLock::new(tls_cfg));
        let watcher_config_clone = shared_tls_config.clone();
        let domains_to_watch = domains.clone();
        tokio::spawn(async move {
            info!(
                "Starting certificate watcher for domains: {:?}",
                domains_to_watch
            );
            if let Err(e) =
                watch_for_certificate_changes(domains_to_watch, watcher_config_clone).await
            {
                error!("Certificate watcher task has failed: {e}");
            }
        });
        let reveiver = &mut self.channel;
        let listener = TcpListener::bind(addr).await?;
        info!("Listening on https://{addr}");
        loop {
            tokio::select! {
                    Ok((tcp_stream,addr))= listener.accept()=>{
                        let tls_acceptor = {
                            let config_guard = shared_tls_config.read().map_err(|e| AppError(format!("Failed to get read lock on TLS config: {e}")))?;
                            TlsAcceptor::from(Arc::new(config_guard.clone()))
                        };
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
                        proxy_adapter(cloned_port,cloned_shared_config.clone(),client.clone(), req, mapping_key2.clone(), addr)
                    });
                    if let Err(err) = http1::Builder::new().serve_connection(io, service).with_upgrades().await {
                        error!("Error serving connection: {err:?}");
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
async fn proxy_adapter<B>(
    port: i32,
    shared_config: SharedConfig,
    client: AppClients,
    req: Request<B>,
    mapping_key: String,
    remote_addr: SocketAddr,
) -> Result<Response<BoxBody<Bytes, AppError>>, AppError>
where
    B: http_body::Body<Data = Bytes> + Send + Sync + std::fmt::Debug + Unpin + 'static,
    AppError: From<B::Error>,
{
    let result =
        proxy_adapter_with_error(port, shared_config, client, req, mapping_key, remote_addr).await;
    match result {
        Ok(res) => Ok(res),
        Err(err) => {
            // Never leak internal error details (config paths, upstream
            // addresses, ...) to the client: log them, answer with 502.
            error!("Failed to handle request: {err}.");
            let body = Full::new(Bytes::from_static(b"{\"error\":\"bad gateway\"}"))
                .map_err(|e| match e {})
                .boxed();
            Ok(Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .header(header::CONTENT_TYPE, "application/json")
                .body(body)?)
        }
    }
}
async fn proxy_adapter_with_error<B>(
    port: i32,
    shared_config: SharedConfig,
    client: AppClients,
    req: Request<B>,
    mapping_key: String,
    remote_addr: SocketAddr,
) -> Result<Response<BoxBody<Bytes, AppError>>, AppError>
where
    B: http_body::Body<Data = Bytes> + Send + Sync + std::fmt::Debug + Unpin + 'static,
    AppError: From<B::Error>,
{
    let method = req.method().clone();
    let uri = req.uri().clone();
    let path = uri
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/")
        .to_string();

    let started = std::time::Instant::now();
    let mut spire_context = SpireContext::new(port, None);

    let res = match proxy(
        port,
        shared_config,
        client,
        req,
        mapping_key.clone(),
        remote_addr,
        CommonCheckRequest {},
        &mut spire_context,
    )
    .await
    {
        Ok(resp) => resp,
        Err(err) => {
            error!("Proxy error for {method} {path}: {err}");
            let body = Full::new(Bytes::from_static(b"{\"error\":\"bad gateway\"}"))
                .map_err(|e| match e {})
                .boxed();
            Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .header(header::CONTENT_TYPE, "application/json")
                .body(body)?
        }
    };

    // Metrics are strictly best effort: a missing/unregistered collector must
    // never change the response the client receives. Labels are bounded: the
    // matched route id, never the raw path (which would create one time series
    // per URL and grow the process without limit).
    let route_label = spire_context.route_label().to_string();
    if let Some(histogram) = metrics::HTTP_REQUEST_DURATION_SECONDS.get() {
        histogram
            .with_label_values(&[mapping_key.as_str(), route_label.as_str(), method.as_str()])
            .observe(started.elapsed().as_secs_f64());
    }
    let status = res.status();
    if let Some(counter) = metrics::HTTP_REQUESTS_TOTAL.get() {
        counter
            .with_label_values(&[
                mapping_key.as_str(),
                route_label.as_str(),
                method.as_str(),
                status.as_str(),
            ])
            .inc();
    }

    // Access log: debug level, because at info level it costs a formatted line
    // per request and is one of the first things to show up in profiles.
    debug!(
        "{} - -  \"{} {} HTTP/1.1\" {}  {:?}",
        remote_addr,
        method,
        path,
        status.as_u16(),
        started.elapsed()
    );
    Ok(res)
}

/// Lazily binds circuit breakers to their route id (set once per shared
/// instance) so state transitions can be exported to metrics.
fn bind_breaker_labels(spire_context: &SpireContext) {
    if let Some(middlewares) = &spire_context.middlewares {
        for middleware in middlewares {
            if let MiddleWares::CircuitBreaker(breaker) = middleware
                && let Ok(mut guard) = breaker.lock()
            {
                guard.set_route_label(spire_context.route_label());
            }
        }
    }
}

#[allow(clippy::too_many_arguments)] // one entry point per request; a context struct would only move the same fields
async fn proxy<B>(
    port: i32,
    shared_config: SharedConfig,
    client: AppClients,
    mut req: Request<B>,
    mapping_key: String,
    remote_addr: SocketAddr,
    chain_trait: impl ChainTrait,
    spire_context: &mut SpireContext,
) -> Result<Response<BoxBody<Bytes, AppError>>, AppError>
where
    B: http_body::Body<Data = Bytes> + Send + Sync + std::fmt::Debug + Unpin + 'static,
    AppError: From<B::Error>,
{
    let inbound_headers = req.headers();
    let cloned_headers = inbound_headers.clone();
    let method = req.method();
    let uri = req.uri().clone();
    let exchange_started = Instant::now();
    let handling_result = chain_trait
        .get_destination(
            shared_config.clone(),
            port,
            method,
            mapping_key.clone(),
            inbound_headers,
            uri,
            remote_addr,
            spire_context,
        )
        .await?;
    debug!("The get_destination is {handling_result:?}");
    // Middlewares must see the resolved client address (behind a trusted proxy
    // the socket peer is the load balancer), not the raw peer.
    let client_addr = spire_context.client_addr.unwrap_or(remote_addr);
    let handling_result = match handling_result {
        DestinationResult::Matched(hr) => hr,
        DestinationResult::NotAllowed(denial) => {
            debug!("Request denied: {denial:?}");
            let mut response = Response::builder().status(denial.status).body(
                Full::new(Bytes::from(denial.body))
                    .map_err(|e| match e {})
                    .boxed(),
            )?;
            response.headers_mut().extend(denial.headers);
            return Ok(response);
        }
        DestinationResult::NoMatchFound => {
            debug!("No match found for the request.");
            let response = Response::builder().status(StatusCode::NOT_FOUND).body(
                Full::new(Bytes::from("Not Found"))
                    .map_err(|e| match e {})
                    .boxed(),
            )?;
            return Ok(response);
        }
    };
    bind_breaker_labels(spire_context);

    let (request_id, route_id_owned, method_owned) = wasm_exchange_ids(spire_context, method);
    let has_plugins = spire_context
        .middlewares
        .as_ref()
        .is_some_and(|m| has_wasm(m));

    if req.method() == Method::OPTIONS
        && req.headers().contains_key(header::ORIGIN)
        && req
            .headers()
            .contains_key(header::ACCESS_CONTROL_REQUEST_METHOD)
        && let Some(cors_config) = spire_context.cors_configed()?
    {
        // Echo the origin the browser actually asked for.
        let origin = req
            .headers()
            .get(header::ORIGIN)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        return chain_trait.handle_preflight(cors_config, &origin);
    }
    let is_websocket_upgrade =
        inbound_headers.contains_key(CONNECTION) && inbound_headers.contains_key(SEC_WEBSOCKET_KEY);
    if is_websocket_upgrade {
        let phase_info = PhaseInfo {
            client_addr,
            route_id: &route_id_owned,
            request_id: &request_id,
            listener_port: port,
        };
        return upgrade_websocket(
            client,
            &chain_trait,
            spire_context,
            req,
            phase_info,
            handling_result,
        )
        .await;
    }

    let check_request = handling_result;
    let request_path = check_request.request_path.as_str();
    let router_destination = check_request.router_destination;
    // Captured for the response phase, which otherwise could not tell a plugin
    // which request produced the response. Only the HTTP arm fills it: the gRPC
    // branch never runs the request phase, so a plugin never saw that request.
    let mut wasm_request: Option<RequestSnapshot> = None;
    let mut res = match router_destination {
        RouterDestination::File(ref _s) => {
            let mut parts = req.uri().clone().into_parts();
            parts.path_and_query = Some(
                request_path
                    .try_into()
                    .map_err(|e: http::uri::InvalidUri| AppError(format!("{e}")))?,
            );
            *req.uri_mut() = Uri::from_parts(parts)
                .map_err(|e: http::uri::InvalidUriParts| AppError(format!("{e}")))?;
            route_file(router_destination, req).await
        }
        RouterDestination::Http(ref base_route) => {
            *req.uri_mut() = request_path
                .parse()
                .map_err(|e: http::uri::InvalidUri| AppError(format!("{e}")))?;
            let host = req
                .uri()
                .host()
                .ok_or("Uri to host cause error")?
                .to_string();
            req.headers_mut()
                .insert(http::header::HOST, HeaderValue::from_str(&host)?);
            let phase_info = PhaseInfo {
                client_addr,
                route_id: &route_id_owned,
                request_id: &request_id,
                listener_port: port,
            };
            if let Some(denial) = apply_request_middlewares(
                &chain_trait,
                spire_context.middlewares.clone(),
                phase_info,
                &mut req,
            )
            .await?
            {
                // A plugin answered without touching the upstream. Same
                // contract as a routing denial: response middlewares do not
                // run on a locally generated reply.
                return denial_response(denial);
            }
            if has_plugins {
                wasm_request = Some(RequestSnapshot::capture(
                    req.method(),
                    req.uri(),
                    req.headers(),
                    client_addr,
                    &route_id_owned,
                    port,
                    &request_id,
                ));
            }
            forward_http_upstream(
                &client,
                req,
                UpstreamCall {
                    endpoint: &base_route.endpoint,
                    timeout_ms: check_request.timeout,
                    concurrency: &check_request.concurrency,
                    mapping_key: &mapping_key,
                    route_label: spire_context.route_label(),
                },
                request_path,
            )
            .await
        }
        RouterDestination::Grpc(s) => {
            forward_grpc_upstream(
                &client,
                req,
                UpstreamCall {
                    endpoint: &s.endpoint,
                    timeout_ms: check_request.timeout,
                    concurrency: &check_request.concurrency,
                    mapping_key: &mapping_key,
                    route_label: spire_context.route_label(),
                },
                request_path,
            )
            .await
        }
    };
    if let Some(middlewares) = spire_context.middlewares.as_mut()
        && !middlewares.is_empty()
    {
        let phase_info = PhaseInfo {
            client_addr,
            route_id: &route_id_owned,
            request_id: &request_id,
            listener_port: port,
        };
        // Wasm plugins see the upstream response before Spire's own response
        // middlewares run, so `compression`, `cors` and header rewriting still
        // apply to whatever the plugin produced. A transport error has no
        // response to show a plugin, so only the `Ok` case is offered to it.
        if let Ok(response) = res.as_mut() {
            apply_response_phases(middlewares, phase_info, wasm_request.as_ref(), response)?;
        }
        chain_trait
            .handle_before_response(middlewares, request_path, &mut res, cloned_headers.clone())
            .await?;
        // Access-log hooks run last: the response is final, and a failure here
        // can no longer change what the client receives.
        if let Ok(response) = res.as_ref() {
            apply_log_phases(
                middlewares,
                phase_info,
                &method_owned,
                request_path,
                response,
                exchange_started.elapsed(),
            );
        }
    }
    res
}

/// Owned per-exchange values the WASM phases need after `req` has been consumed
/// by the upstream call: a request id, the route id and the method.
///
/// Paid for only when the matched route actually configures a plugin; otherwise
/// all three stay empty.
fn wasm_exchange_ids(spire_context: &SpireContext, method: &Method) -> (String, String, String) {
    let has_plugins = spire_context
        .middlewares
        .as_ref()
        .is_some_and(|m| has_wasm(m));
    if !has_plugins {
        return (String::new(), String::new(), String::new());
    }
    (
        crate::utils::uuid::get_uuid(),
        spire_context.route_label().to_string(),
        method.as_str().to_string(),
    )
}

/// Handles a WebSocket upgrade: the request-phase middlewares run first (a
/// plugin may still answer locally), then the upgrade goes to the upstream.
async fn upgrade_websocket<B>(
    client: AppClients,
    chain_trait: &impl ChainTrait,
    spire_context: &SpireContext,
    mut req: Request<B>,
    info: PhaseInfo<'_>,
    handling_result: HandlingResult,
) -> Result<Response<BoxBody<Bytes, AppError>>, AppError>
where
    B: http_body::Body<Data = Bytes> + Send + Sync + std::fmt::Debug + Unpin + 'static,
    AppError: From<B::Error>,
{
    debug!("The request has been updated to websocket,the req is {req:?}!");
    if let Some(denial) = apply_request_middlewares(
        chain_trait,
        spire_context.middlewares.clone(),
        info,
        &mut req,
    )
    .await?
    {
        return denial_response(denial);
    }
    server_upgrade(req, handling_result, client.http).await
}

/// Serves a static file route.
async fn route_file<B>(
    router_destination: RouterDestination,
    req: Request<B>,
) -> Result<Response<BoxBody<Bytes, AppError>>, AppError> {
    let static_ = Static::new(Path::new(router_destination.get_endpoint().as_str()));
    static_
        .clone()
        .serve(req)
        .await
        .map(|item| {
            item.map(|body| {
                body.boxed()
                    // A read error while streaming a file must surface as an
                    // error response, never as a panic in the request task.
                    .map_err(|e| AppError(format!("Failed to stream file: {e}")))
                    .boxed()
            })
        })
        .map_err(AppError::from)
}
#[cfg(test)]
mod tests {
    use super::*;

    use crate::middleware::authentication::BasicAuth;
    use crate::middleware::forward_header::ForwardHeader;
    use crate::middleware::middlewares::MiddleWares;
    use crate::middleware::request_headers::RequestHeaders;
    use crate::proxy::http1::upstream_forward::gateway_timeout_response;
    use crate::proxy::http1::upstream_forward::service_unavailable_response;
    use crate::proxy::proxy_trait::{HandlingResult, MockChainTrait};
    use crate::vojo::app_config::AppConfig;
    use crate::vojo::app_config::{ApiService, RouteConfig};
    use crate::vojo::matcher::MatcherRule;
    use crate::vojo::matcher::PathMatchType;
    use crate::vojo::router::StaticFileRoute;
    use crate::vojo::router::{BaseRoute, RandomRoute, Router};
    use http::HeaderMap;
    use std::collections::HashMap;
    use std::convert::Infallible;
    use std::net::IpAddr;
    use std::net::Ipv4Addr;
    use std::sync::Arc;
    use std::sync::Mutex;
    #[test]
    fn test_http_proxy_creation() {
        let (_, rx) = mpsc::channel(1);
        let shared_config = SharedConfig::from_app_config(AppConfig::default());

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
        // `proxy_adapter` reads the global metrics registry, which the app
        // bootstrap normally initializes. Without it every request would be
        // answered with 404 instead of reaching the proxy logic.
        let _ = crate::monitor::prometheus_exporter::metrics::init();

        let shared_config = SharedConfig::from_app_config(AppConfig::default());
        let client = AppClients::new(shared_config.clone(), 3302).await.unwrap();

        let remote_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);

        let req = Request::builder()
            .uri("invalid://uri")
            .body(
                Full::new(Bytes::from("test"))
                    .map_err(AppError::from)
                    .boxed(),
            )
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
        // Unroutable/failed upstream requests are gateway errors, not 404s.
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }
    #[tokio::test]
    async fn test_options_preflight_request() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://127.0.0.1:8080"),
        );
        headers.insert(
            header::ACCESS_CONTROL_REQUEST_METHOD,
            HeaderValue::from_static("POST"),
        );

        let shared_config = SharedConfig::from_app_config(AppConfig {
            api_service_config: HashMap::from([(
                8080,
                ApiService {
                    listen_port: 8080,
                    route_configs: vec![RouteConfig {
                        router: Router::Random(RandomRoute {
                            routes: vec![BaseRoute {
                                endpoint: "http://127.0.0.1:9394".to_string(),
                                ..Default::default()
                            }],
                        }),
                        matchers: vec![MatcherRule::Path {
                            value: "/".to_string(),
                            match_type: crate::vojo::matcher::PathMatchType::Exact,
                            regex: Default::default(),
                        }],

                        ..Default::default()
                    }],
                    ..Default::default()
                },
            )]),
            ..Default::default()
        });
        let client = AppClients::new(shared_config.clone(), 8080).await.unwrap();

        let remote_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);

        let mut req = Request::builder()
            .method(Method::OPTIONS)
            .uri("http://127.0.0.1:8080/test")
            .body(Full::new(Bytes::from("")).map_err(AppError::from).boxed())
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
            &mut SpireContext::new(8080, None),
        )
        .await;
        println!("result is {result:?}");
        assert!(result.is_ok());
    }
    #[tokio::test]
    async fn test_proxy_handling_result_none() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://127.0.0.1:8080"),
        );
        headers.insert(
            header::ACCESS_CONTROL_REQUEST_METHOD,
            HeaderValue::from_static("POST"),
        );

        let shared_config = SharedConfig::from_app_config(AppConfig::default());
        let client = AppClients::new(shared_config.clone(), 8080).await.unwrap();

        let remote_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);

        let mut req = Request::builder()
            .method(Method::OPTIONS)
            .uri("http://127.0.0.1:8080/test")
            .body(Full::new(Bytes::from("")).map_err(AppError::from).boxed())
            .unwrap();
        req.headers_mut().extend(headers);

        let mut mock_chain_trait = MockChainTrait::new();
        mock_chain_trait
            .expect_get_destination()
            .returning(|_, _, _, _, _, _, _, _| {
                Ok(crate::proxy::proxy_trait::DestinationResult::NoMatchFound)
            });
        let result = proxy(
            8080,
            shared_config,
            client,
            req,
            "test".to_string(),
            remote_addr,
            mock_chain_trait,
            &mut SpireContext::new(8080, None),
        )
        .await;
        println!("result is {result:?}");
        assert!(result.is_ok());
    }
    #[tokio::test]
    async fn test_proxy_middle() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://127.0.0.1:8080"),
        );
        headers.insert(
            header::ACCESS_CONTROL_REQUEST_METHOD,
            HeaderValue::from_static("POST"),
        );

        let shared_config = SharedConfig::from_app_config(AppConfig::default());
        let client = AppClients::new(shared_config.clone(), 8080).await.unwrap();

        let remote_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);

        let mut req = Request::builder()
            .method(Method::OPTIONS)
            .uri("http://127.0.0.1:8080/test")
            .body(Full::new(Bytes::from("")).map_err(AppError::from).boxed())
            .unwrap();
        req.headers_mut().extend(headers);

        let mut mock_chain_trait = MockChainTrait::new();
        mock_chain_trait.expect_get_destination().returning(
            |_, _, _, _, _, _, _, spire_context| {
                spire_context.middlewares = Some(vec![MiddleWares::Authentication(
                    crate::middleware::authentication::Authentication::Basic(BasicAuth {
                        credentials: "user:pass".to_string(),
                    }),
                )]);

                Ok(crate::proxy::proxy_trait::DestinationResult::Matched(
                    HandlingResult {
                        request_path: "/test".to_string(),
                        router_destination: RouterDestination::File(StaticFileRoute {
                            doc_root: "./test".to_string(),
                        }),
                        timeout: 1000,
                        concurrency: crate::vojo::concurrency::ConcurrencyConfig::global_default(
                            1024,
                        ),
                    },
                ))
            },
        );
        mock_chain_trait
            .expect_handle_before_request()
            .returning(|_, _, _| Err(AppError("test".to_string())));
        mock_chain_trait
            .expect_handle_before_response()
            .returning(|_, _, _, _| Err(AppError("test".to_string())));
        let result = proxy(
            8080,
            shared_config,
            client,
            req,
            "test".to_string(),
            remote_addr,
            mock_chain_trait,
            &mut SpireContext::new(8080, None),
        )
        .await;
        println!("result is {result:?}");
        assert!(result.is_err());
    }
    #[tokio::test]
    async fn test_proxy_route_file() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://127.0.0.1:8080"),
        );
        headers.insert(
            header::ACCESS_CONTROL_REQUEST_METHOD,
            HeaderValue::from_static("POST"),
        );

        let shared_config = SharedConfig::from_app_config(AppConfig::default());
        let client = AppClients::new(shared_config.clone(), 8080).await.unwrap();

        let remote_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);

        let mut req = Request::builder()
            .method(Method::OPTIONS)
            .uri("http://127.0.0.1:8080/test")
            .body(Full::new(Bytes::from("")).map_err(AppError::from).boxed())
            .unwrap();
        req.headers_mut().extend(headers);

        let mut mock_chain_trait = MockChainTrait::new();
        mock_chain_trait
            .expect_get_destination()
            .returning(|_, _, _, _, _, _, _, _| {
                Ok(crate::proxy::proxy_trait::DestinationResult::Matched(
                    HandlingResult {
                        request_path: "/test".to_string(),
                        router_destination: RouterDestination::File(StaticFileRoute {
                            doc_root: "./test".to_string(),
                        }),
                        timeout: 1000,
                        concurrency: crate::vojo::concurrency::ConcurrencyConfig::global_default(
                            1024,
                        ),
                    },
                ))
            });
        mock_chain_trait
            .expect_handle_before_request()
            .returning(|_, _, _| Err(AppError("test".to_string())));
        let result = proxy(
            8080,
            shared_config,
            client,
            req,
            "test".to_string(),
            remote_addr,
            mock_chain_trait,
            &mut SpireContext::new(8080, None),
        )
        .await;
        println!("result is {result:?}");
        assert!(result.is_ok());
    }
    #[tokio::test]
    async fn test_route_file() {
        let router_destination = RouterDestination::File(StaticFileRoute {
            doc_root: "./test".to_string(),
        });

        let req = Request::builder()
            .uri("http://localhost/test.txt")
            .body(Full::new(Bytes::from("")).map_err(AppError::from).boxed())
            .unwrap();

        let result = route_file(router_destination, req).await;
        assert!(result.is_ok());
    }

    type RecordedRequests = Arc<Mutex<Vec<(String, HeaderMap)>>>;

    /// Minimal upstream that records the request line path and headers, then
    /// answers 200. Used to observe what Spire actually forwards.
    async fn spawn_recording_upstream() -> (SocketAddr, RecordedRequests) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let recorded: RecordedRequests = Arc::new(Mutex::new(Vec::new()));
        let recorded_for_server = recorded.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let recorded = recorded_for_server.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |req: Request<Incoming>| {
                        let recorded = recorded.clone();
                        async move {
                            recorded
                                .lock()
                                .unwrap()
                                .push((req.uri().path().to_string(), req.headers().clone()));
                            Ok::<_, Infallible>(Response::new(Full::new(Bytes::new())))
                        }
                    });
                    let io = TokioIo::new(stream);
                    if let Err(err) = http1::Builder::new().serve_connection(io, service).await {
                        error!("recording upstream connection error: {err:?}");
                    }
                });
            }
        });
        (addr, recorded)
    }

    fn forward_header_config(upstream: SocketAddr) -> SharedConfig {
        let route = RouteConfig {
            matchers: vec![MatcherRule::Path {
                value: "/tungate".to_string(),
                match_type: PathMatchType::Prefix,
                regex: Default::default(),
            }],
            router: Router::Random(RandomRoute::new(vec![format!("http://{upstream}")])),
            middlewares: Some(vec![
                MiddleWares::ForwardHeader(ForwardHeader {}),
                MiddleWares::RequestHeaders(RequestHeaders {
                    add: HashMap::from([(
                        "X-Forwarded-Host".to_string(),
                        "www.937453.xyz".to_string(),
                    )]),
                    remove: Vec::new(),
                }),
            ]),
            ..Default::default()
        };
        let mut app_config = AppConfig::default();
        app_config.api_service_config.insert(
            8080,
            ApiService {
                listen_port: 8080,
                route_configs: vec![route],
                ..Default::default()
            },
        );
        SharedConfig::from_app_config(app_config)
    }

    fn upgrade_request() -> Request<BoxBody<Bytes, AppError>> {
        Request::builder()
            .method(Method::GET)
            .uri("/tungate/tunnel/ws")
            .header(CONNECTION, "upgrade")
            .header(header::UPGRADE, "websocket")
            .header(SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ==")
            .header(header::SEC_WEBSOCKET_VERSION, "13")
            .header(header::HOST, "www.937453.xyz")
            .body(Full::new(Bytes::new()).map_err(AppError::from).boxed())
            .unwrap()
    }

    #[tokio::test]
    async fn test_websocket_upgrade_applies_forward_headers() {
        let (upstream_addr, recorded) = spawn_recording_upstream().await;
        let shared_config = forward_header_config(upstream_addr);
        let client = AppClients::new(shared_config.clone(), 8080).await.unwrap();

        // A non-loopback peer proves the address comes from the connection, not
        // from the upstream socket.
        let remote_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9)), 54321);

        let result = proxy(
            8080,
            shared_config,
            client,
            upgrade_request(),
            "test".to_string(),
            remote_addr,
            CommonCheckRequest {},
            &mut SpireContext::new(8080, None),
        )
        .await
        .expect("websocket upgrade request should be proxied");
        assert_eq!(result.status(), StatusCode::OK);

        let recorded = recorded.lock().unwrap();
        let (path, headers) = recorded
            .last()
            .expect("upstream should receive the websocket upgrade request");
        assert_eq!(path, "/tungate/tunnel/ws");
        assert_eq!(headers.get("x-real-ip").unwrap(), "203.0.113.9");
        assert_eq!(headers.get("x-forwarded-for").unwrap(), "203.0.113.9");
        assert_eq!(headers.get("x-forwarded-host").unwrap(), "www.937453.xyz");
        assert_eq!(headers.get(header::UPGRADE).unwrap(), "websocket");
    }

    #[serial_test::serial]
    #[tokio::test]
    async fn test_http_request_applies_forward_headers() {
        let (upstream_addr, recorded) = spawn_recording_upstream().await;
        let shared_config = forward_header_config(upstream_addr);
        let client = AppClients::new(shared_config.clone(), 8080).await.unwrap();

        let remote_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9)), 54321);

        let req = Request::builder()
            .method(Method::GET)
            .uri("/tungate/ping")
            .header(header::HOST, "www.937453.xyz")
            .body(Full::new(Bytes::new()).map_err(AppError::from).boxed())
            .unwrap();

        let result = proxy(
            8080,
            shared_config,
            client,
            req,
            "test".to_string(),
            remote_addr,
            CommonCheckRequest {},
            &mut SpireContext::new(8080, None),
        )
        .await
        .expect("plain http request should be proxied");
        assert_eq!(result.status(), StatusCode::OK);

        let recorded = recorded.lock().unwrap();
        let (path, headers) = recorded
            .last()
            .expect("upstream should receive the plain http request");
        assert_eq!(path, "/tungate/ping");
        assert_eq!(headers.get("x-real-ip").unwrap(), "203.0.113.9");
        assert_eq!(headers.get("x-forwarded-for").unwrap(), "203.0.113.9");
        assert_eq!(headers.get("x-forwarded-host").unwrap(), "www.937453.xyz");
    }

    // ------------------------------------------------------------------
    // Slow-downstream protection: timeouts, breaker visibility, backpressure
    // ------------------------------------------------------------------

    use crate::proxy::concurrency;
    use crate::vojo::concurrency::ConcurrencyConfig;
    use crate::vojo::timeout_config::TimeoutConfig;
    use http_body::Frame;
    use std::pin::Pin;
    use std::task::Context;
    use std::task::Poll;
    use std::time::Duration;

    /// Upstream that accepts immediately but answers after `delay` — the
    /// "slow interface" under test.
    async fn spawn_slow_upstream(delay: Duration) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let service = service_fn(move |_: Request<Incoming>| async move {
                        tokio::time::sleep(delay).await;
                        Ok::<_, Infallible>(Response::new(Full::new(Bytes::new())))
                    });
                    let io = TokioIo::new(stream);
                    if let Err(err) = http1::Builder::new().serve_connection(io, service).await {
                        error!("slow upstream connection error: {err:?}");
                    }
                });
            }
        });
        addr
    }

    /// Upstream that answers 200 with one body chunk and then goes silent —
    /// the "slow-drip body" under test.
    struct OneChunkThenStallBody {
        chunk: Bytes,
        delivered: bool,
    }
    impl http_body::Body for OneChunkThenStallBody {
        type Data = Bytes;
        type Error = Infallible;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
            if !self.delivered {
                self.delivered = true;
                Poll::Ready(Some(Ok(Frame::data(self.chunk.clone()))))
            } else {
                Poll::Pending
            }
        }
    }

    async fn spawn_stalling_body_upstream() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let service = service_fn(|_: Request<Incoming>| async {
                        Ok::<_, Infallible>(Response::new(OneChunkThenStallBody {
                            chunk: Bytes::from_static(b"partial"),
                            delivered: false,
                        }))
                    });
                    let io = TokioIo::new(stream);
                    if let Err(err) = http1::Builder::new().serve_connection(io, service).await {
                        error!("stalling upstream connection error: {err:?}");
                    }
                });
            }
        });
        addr
    }

    fn upstream_route_config(
        upstream: SocketAddr,
        timeout_ms: u64,
        concurrency: Option<ConcurrencyConfig>,
        middlewares: Option<Vec<MiddleWares>>,
        route_id: &str,
    ) -> SharedConfig {
        let route = RouteConfig {
            route_id: route_id.to_string(),
            matchers: vec![MatcherRule::Path {
                value: "/".to_string(),
                match_type: PathMatchType::Prefix,
                regex: Default::default(),
            }],
            router: Router::Random(RandomRoute::new(vec![format!("http://{upstream}")])),
            timeout: Some(TimeoutConfig {
                request_timeout: timeout_ms,
            }),
            concurrency,
            middlewares,
            ..Default::default()
        };
        let mut app_config = AppConfig::default();
        app_config.api_service_config.insert(
            8080,
            ApiService {
                listen_port: 8080,
                route_configs: vec![route],
                ..Default::default()
            },
        );
        SharedConfig::from_app_config(app_config)
    }

    async fn proxy_get(
        shared_config: &SharedConfig,
        path: &str,
        mapping_key: &str,
    ) -> Result<Response<BoxBody<Bytes, AppError>>, AppError> {
        let client = AppClients::new(shared_config.clone(), 8080).await.unwrap();
        let req = Request::builder()
            .method(Method::GET)
            .uri(path)
            .body(Full::new(Bytes::new()).map_err(AppError::from).boxed())
            .unwrap();
        let remote_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
        proxy(
            8080,
            shared_config.clone(),
            client,
            req,
            mapping_key.to_string(),
            remote_addr,
            CommonCheckRequest {},
            &mut SpireContext::new(8080, None),
        )
        .await
    }

    #[serial_test::serial]
    #[tokio::test]
    async fn test_upstream_timeout_returns_504() {
        let _ = crate::monitor::prometheus_exporter::metrics::init();
        let upstream = spawn_slow_upstream(Duration::from_millis(1_000)).await;
        let shared_config = upstream_route_config(upstream, 100, None, None, "timeout-route");

        let res = proxy_get(&shared_config, "/slow", "test").await.unwrap();
        assert_eq!(res.status(), StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(res.headers().get(header::RETRY_AFTER).unwrap(), "1");

        // The timeout is counted for alerting.
        if let Some(counter) =
            crate::monitor::prometheus_exporter::metrics::SPIRE_UPSTREAM_TIMEOUTS_TOTAL.get()
        {
            assert!(counter.with_label_values(&["test", "timeout-route"]).get() >= 1.0);
        }
    }

    #[serial_test::serial]
    #[tokio::test]
    async fn test_timeout_trips_circuit_breaker() {
        let _ = crate::monitor::prometheus_exporter::metrics::init();
        let upstream = spawn_slow_upstream(Duration::from_millis(1_000)).await;
        // Timeouts previously bypassed the breaker entirely; three in a row
        // must open it now.
        let breaker: MiddleWares = serde_yaml::from_str(
            "kind: circuit_breaker\nfailure_threshold: 1.0\nconsecutive_failures: 3\ncooldown: 60s\nhalf_open_requests: 1\nrequest_volume_threshold: 5",
        )
        .unwrap();
        let shared_config =
            upstream_route_config(upstream, 80, None, Some(vec![breaker]), "cb-timeout-route");

        for _ in 0..3 {
            let res = proxy_get(&shared_config, "/slow", "test").await.unwrap();
            assert_eq!(res.status(), StatusCode::GATEWAY_TIMEOUT);
        }
        // Fourth request: the breaker is open — denied before the upstream.
        let denied = proxy_get(&shared_config, "/slow", "test").await.unwrap();
        assert_eq!(denied.status(), StatusCode::SERVICE_UNAVAILABLE);

        // And the state is exported for alerting (1 = open).
        if let Some(gauge) =
            crate::monitor::prometheus_exporter::metrics::SPIRE_CIRCUIT_BREAKER_STATE.get()
        {
            assert_eq!(gauge.with_label_values(&["cb-timeout-route"]).get(), 1);
        }
    }

    #[serial_test::serial]
    #[tokio::test]
    async fn test_concurrency_limit_rejects_second_in_flight_request() {
        let _ = crate::monitor::prometheus_exporter::metrics::init();
        let upstream = spawn_slow_upstream(Duration::from_millis(400)).await;
        let shared_config = upstream_route_config(
            upstream,
            5_000,
            Some(ConcurrencyConfig {
                limit: 1,
                queue_length: 0,
                queue_timeout_ms: 50,
            }),
            None,
            "conc-route",
        );

        let cfg_first = shared_config.clone();
        let first = tokio::spawn(async move { proxy_get(&cfg_first, "/slow", "test").await });
        // Let the first request acquire the permit and park on the upstream.
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Fail fast: the second request must not queue behind the slow one.
        let second = proxy_get(&shared_config, "/slow", "test").await.unwrap();
        assert_eq!(second.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(second.headers().contains_key(header::RETRY_AFTER));

        let endpoint = concurrency::endpoint_key(&format!("http://{upstream}"));
        if let Some(counter) =
            crate::monitor::prometheus_exporter::metrics::SPIRE_UPSTREAM_CONCURRENCY_REJECTED_TOTAL
                .get()
        {
            assert!(
                counter
                    .with_label_values(&["test", "conc-route", endpoint.as_str()])
                    .get()
                    >= 1.0
            );
        }

        // The first request finishes normally once the upstream answers.
        let first_resp = first.await.unwrap().unwrap();
        assert_eq!(first_resp.status(), StatusCode::OK);
        // Drain the body: only then is the permit released and in-flight
        // back to zero.
        let _ = first_resp.into_body().collect().await.unwrap();
        if let Some(gauge) =
            crate::monitor::prometheus_exporter::metrics::SPIRE_UPSTREAM_IN_FLIGHT.get()
        {
            assert_eq!(
                gauge
                    .with_label_values(&["test", "conc-route", endpoint.as_str()])
                    .get(),
                0
            );
        }
    }

    #[serial_test::serial]
    #[tokio::test]
    async fn test_upstream_body_idle_timeout_cuts_stream() {
        let _ = crate::monitor::prometheus_exporter::metrics::init();
        let upstream = spawn_stalling_body_upstream().await;
        let shared_config = upstream_route_config(upstream, 150, None, None, "idle-route");

        let response = proxy_get(&shared_config, "/stream", "test").await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body();
        let first = body.frame().await.expect("first chunk").expect("ok frame");
        assert_eq!(first.data_ref().unwrap(), &Bytes::from_static(b"partial"));

        // No second frame ever comes; the idle watchdog must resolve the
        // stall with an error instead of parking the request forever.
        let started = std::time::Instant::now();
        let second = body.frame().await;
        let err = second
            .expect("watchdog must resolve the stall")
            .expect_err("must be an error");
        assert!(err.0.contains("idle timeout"));
        assert!(started.elapsed() < Duration::from_secs(5));

        // Permit released, in-flight back to zero after the stream was cut.
        let endpoint = concurrency::endpoint_key(&format!("http://{upstream}"));
        if let Some(gauge) =
            crate::monitor::prometheus_exporter::metrics::SPIRE_UPSTREAM_IN_FLIGHT.get()
        {
            assert_eq!(
                gauge
                    .with_label_values(&["test", "idle-route", endpoint.as_str()])
                    .get(),
                0
            );
        }
    }

    #[test]
    fn gateway_timeout_response_shape() {
        let res = gateway_timeout_response().unwrap();
        assert_eq!(res.status(), StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(res.headers().get(header::RETRY_AFTER).unwrap(), "1");
        let res = service_unavailable_response().unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(res.headers().get(header::RETRY_AFTER).unwrap(), "1");
    }
}
