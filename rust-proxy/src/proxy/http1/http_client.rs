use crate::proxy::http1::req_body::ProxyRequestBody;

use hyper::Request;
use hyper_util::client::legacy::ResponseFuture;
use hyper_util::client::legacy::connect::Connect;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use rustls::RootCertStore;
use std::time::Duration;
use tokio::time::Timeout;
use tokio::time::timeout;

#[derive(Clone)]
pub struct HttpClients {
    pub http_client: Client<HttpConnector, ProxyRequestBody>,
    pub https_client: Client<hyper_rustls::HttpsConnector<HttpConnector>, ProxyRequestBody>,
}

/// One place for the pool and HTTP/2 settings, so the plain-HTTP and TLS
/// clients cannot drift apart (they used to: only the plain client got the
/// pool tuning).
fn build_client<C, B>(connector: C) -> Client<C, B>
where
    C: Connect + Clone + Send + Sync + 'static,
    B: http_body::Body + Send + 'static + Unpin,
    B::Data: Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    Client::builder(TokioExecutor::new())
        .pool_idle_timeout(Duration::from_secs(90))
        .pool_max_idle_per_host(1024)
        .http2_adaptive_window(true)
        .http2_keep_alive_interval(Duration::from_secs(15))
        .http2_keep_alive_timeout(Duration::from_secs(5))
        .build(connector)
}

impl HttpClients {
    pub fn new() -> HttpClients {
        // hyper-util's HttpConnector leaves TCP_NODELAY off. For a reverse proxy
        // that writes request headers and body as separate segments this costs a
        // delayed-ACK round trip per request, so enable it to match nginx.
        let mut connector = HttpConnector::new();
        connector.set_nodelay(true);
        // The HTTPS connector wraps this same connector, so TLS upstreams get
        // TCP_NODELAY too (a fresh `HttpsConnectorBuilder::new()` would build a
        // second, default-configured HttpConnector and silently lose it).
        // `enforce_http(false)` is what `wrap_connector` requires.
        connector.enforce_http(false);

        let http_client = build_client(connector.clone());

        let mut root_store = RootCertStore::empty();
        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

        let tls = rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls)
            .https_or_http()
            .enable_http1()
            .wrap_connector(connector);
        let https_client = build_client(https);
        HttpClients {
            http_client,
            https_client,
        }
    }

    pub fn request_http(
        &self,
        req: Request<ProxyRequestBody>,
        time_out: u64,
    ) -> Timeout<ResponseFuture> {
        let request_future = self.http_client.request(req);
        timeout(Duration::from_millis(time_out), request_future)
    }
    pub fn request_https(
        &self,
        req: Request<ProxyRequestBody>,
        time_out: u64,
    ) -> Timeout<ResponseFuture> {
        let request_future = self.https_client.request(req);
        timeout(Duration::from_millis(time_out), request_future)
    }
}
