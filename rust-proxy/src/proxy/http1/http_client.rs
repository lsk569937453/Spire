use bytes::Bytes;
use http_body_util::combinators::BoxBody;

use crate::vojo::app_error::AppError;
use hyper::Request;
use hyper_util::client::legacy::ResponseFuture;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use rustls::RootCertStore;
use std::time::Duration;
use tokio::time::Timeout;
use tokio::time::timeout;

#[derive(Clone)]
pub struct HttpClients {
    pub http_client: Client<HttpConnector, BoxBody<Bytes, AppError>>,
    pub https_client: Client<hyper_rustls::HttpsConnector<HttpConnector>, BoxBody<Bytes, AppError>>,
}
impl HttpClients {
    pub fn new() -> HttpClients {
        let http_client = Client::builder(TokioExecutor::new())
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(1024)
            .http2_adaptive_window(true)
            .http2_keep_alive_interval(Duration::from_secs(15))
            .http2_keep_alive_timeout(Duration::from_secs(5))
            .http1_title_case_headers(true)
            .http1_preserve_header_case(true)
            .build_http();
        let mut root_store = RootCertStore::empty();
        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

        let tls = rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls)
            .https_or_http()
            .enable_http1()
            .build();
        let https_client = Client::builder(TokioExecutor::new()).build(https);
        HttpClients {
            http_client,
            https_client,
        }
    }

    pub fn request_http(
        &self,
        req: Request<BoxBody<Bytes, AppError>>,
        time_out: u64,
    ) -> Timeout<ResponseFuture> {
        let request_future = self.http_client.request(req);
        timeout(Duration::from_millis(time_out), request_future)
    }
    pub fn request_https(
        &self,
        req: Request<BoxBody<Bytes, AppError>>,
        time_out: u64,
    ) -> Timeout<ResponseFuture> {
        let request_future = self.https_client.request(req);
        timeout(Duration::from_millis(time_out), request_future)
    }
}
