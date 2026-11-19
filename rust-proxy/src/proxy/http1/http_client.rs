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

/// How many idle upstream connections one endpoint may keep for reuse.
///
/// hyper's pool is a *reuse cache*, not a correctness requirement: a request
/// that finds no idle connection simply opens a new one. It is, however, the
/// reason RSS stays at its peak after traffic drops — every pooled connection
/// keeps its hyper read/write buffers (and, on TLS endpoints, its session
/// buffers) alive. The HTTP client used to cap this at 1024 while the HTTPS
/// client ran with hyper's default `usize::MAX`, i.e. it kept *every*
/// connection of the last burst. Both are now explicitly bounded, which keeps
/// the reuse benefit without letting a burst pin an unbounded connection set.
pub const POOL_MAX_IDLE_PER_HOST: usize = 1024;

/// How long an idle upstream connection is kept before it is dropped.
///
/// This, not the pool size, is the knob that decides how long a finished burst
/// keeps its memory: hyper's default of 90 s means a spike is still resident a
/// minute and a half after it ended. 20 s keeps normal keep-alive traffic fully
/// reused (request gaps are milliseconds) while releasing a burst's pooled
/// connections — and their buffers — promptly.
pub const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone)]
pub struct HttpClients {
    pub http_client: Client<HttpConnector, BoxBody<Bytes, AppError>>,
    pub https_client: Client<hyper_rustls::HttpsConnector<HttpConnector>, BoxBody<Bytes, AppError>>,
}
impl HttpClients {
    pub fn new() -> HttpClients {
        let http_client = Client::builder(TokioExecutor::new())
            .pool_idle_timeout(POOL_IDLE_TIMEOUT)
            .pool_max_idle_per_host(POOL_MAX_IDLE_PER_HOST)
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
        // Same bounded pool as the plaintext client. This used to rely on
        // hyper's defaults, which keep *every* idle connection
        // (`pool_max_idle_per_host` = `usize::MAX`) — on HTTPS endpoints each
        // one also pins its TLS session buffers, so a burst left the whole
        // connection set (and its memory) resident.
        let https_client = Client::builder(TokioExecutor::new())
            .pool_idle_timeout(POOL_IDLE_TIMEOUT)
            .pool_max_idle_per_host(POOL_MAX_IDLE_PER_HOST)
            .build(https);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstream_idle_pool_is_bounded() {
        // Regression guard: hyper's defaults (`usize::MAX` idle connections,
        // 90 s timeout) are exactly what kept RSS at its post-burst peak.
        // Both clients must stay bounded so a finished burst releases its
        // pooled connections instead of pinning their buffers.
        const { assert!(POOL_MAX_IDLE_PER_HOST <= 1024) };
        assert!(
            POOL_IDLE_TIMEOUT.as_secs() <= 30,
            "an idle upstream connection must not outlive its usefulness"
        );
    }
}
