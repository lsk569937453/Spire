//! The `host.http-call` implementation.
//!
//! A guest call is synchronous, so an outbound request has to run on the thread
//! already inside the guest. Two things make that safe enough for the short
//! auth/token exchanges this is meant for:
//!
//! * the URL must name a host the operator listed in `http_allowed_hosts`
//!   (empty means "no host"), which is the SSRF boundary: the guest cannot
//!   choose a destination the route did not allow;
//! * the whole exchange is bounded by `http_timeout_ms`, the response body by
//!   `max_http_body_bytes`, and redirects are never followed.
//!
//! The cost is explicit: the worker thread is blocked for the duration of the
//! call. That is the same trade-off as `tokio::task::block_in_place` anywhere
//! else, and it is why the call is a *capability* with operator-controlled
//! timeouts rather than something every plugin can do by default.

use bytes::Bytes;
use http::HeaderMap;
use http::Method;
use http::Request;
use http::header::HeaderName;
use http::header::HeaderValue;
use http_body_util::BodyExt;
use http_body_util::Full;
use std::sync::OnceLock;

use crate::proxy::http1::http_client::HttpClients;
use crate::proxy::http1::req_body::ProxyRequestBody;
use crate::wasm::bindings::spire::filter::types as wit;
use crate::wasm::convert;
use crate::wasm::state::PluginLimits;

/// A guest may not hand the host an unbounded header map.
const MAX_HTTP_HEADERS: usize = 64;
const MAX_HTTP_HEADER_BYTES: usize = 8 * 1024;

/// Headers a client must not forward: they describe a single hop, or would let
/// the guest lie about the request it is making.
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "host",
    "content-length",
];

/// One client for every plugin call in the process, created on first use.
fn client() -> &'static HttpClients {
    static CLIENT: OnceLock<HttpClients> = OnceLock::new();
    CLIENT.get_or_init(HttpClients::new)
}

/// Performs one request/response exchange on behalf of a guest.
///
/// The caller has already checked the capability grant; this function checks
/// the destination allowlist, then delegates to the same blocking bridge the
/// rest of the host uses.
pub fn call(
    limits: &PluginLimits,
    request: wit::HttpRequest,
) -> Result<wit::HttpResponse, wit::Error> {
    let mut url = url::Url::parse(&request.url)
        .map_err(|e| wit::Error::Invalid(format!("http-call: invalid URL: {e}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(wit::Error::Invalid(format!(
            "http-call: scheme `{}` is not http or https",
            url.scheme()
        )));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(wit::Error::Invalid(
            "http-call: URLs with userinfo are refused".to_string(),
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| wit::Error::Invalid("http-call: URL has no host".to_string()))?
        .to_ascii_lowercase();
    let default_port = match url.scheme() {
        "http" => 80,
        _ => 443,
    };
    let effective_port = url.port().unwrap_or(default_port);
    let allowed = limits.http_allowed_hosts.iter().any(|entry| {
        // `host` or `host:port`; an entry without a port also matches the
        // scheme's default port, so `auth.internal` covers
        // `https://auth.internal:443` and not only `https://auth.internal`.
        match entry.rsplit_once(':') {
            Some((entry_host, entry_port)) if entry_port.parse::<u16>().is_ok() => {
                entry_host == host && entry_port.parse::<u16>() == Ok(effective_port)
            }
            _ => entry == &host,
        }
    });
    if !allowed {
        return Err(wit::Error::Denied(format!(
            "http-call: `{host}:{effective_port}` is not in http_allowed_hosts"
        )));
    }
    // A fragment is client-side only; dropping it keeps `hyper` from rejecting
    // the URI and is what a normal HTTP client would do.
    url.set_fragment(None);

    let method = Method::from_bytes(request.method.as_bytes())
        .map_err(|e| wit::Error::Invalid(format!("http-call: invalid method: {e}")))?;
    let headers = to_header_map(&request.headers)?;
    let body = request.body.map(Bytes::from).unwrap_or_default();
    if body.len() > limits.max_http_body_bytes {
        return Err(wit::Error::Invalid(format!(
            "http-call: request body is {} bytes, over the {} byte limit",
            body.len(),
            limits.max_http_body_bytes
        )));
    }

    let mut builder = Request::builder().method(method).uri(url.as_str());
    if let Some(headers_mut) = builder.headers_mut() {
        headers_mut.extend(headers);
    }
    let request = builder
        .body(ProxyRequestBody::Boxed(
            Full::new(body).map_err(|e| match e {}).boxed(),
        ))
        .map_err(|e| wit::Error::Invalid(format!("http-call: cannot build request: {e}")))?;

    run(limits, request)
}

/// Bridges the synchronous guest call onto the async client.
///
/// `block_in_place` moves the other tasks off this worker; `Handle::block_on`
/// then drives the request. The future borrows no `Store` state, so it does not
/// need to be `Send`, but the runtime must be multi-threaded.
fn run(
    limits: &PluginLimits,
    request: Request<ProxyRequestBody>,
) -> Result<wit::HttpResponse, wit::Error> {
    let handle = tokio::runtime::Handle::try_current().map_err(|_| {
        wit::Error::Failed("http-call: no Tokio runtime is running on this thread".to_string())
    })?;
    if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::CurrentThread {
        return Err(wit::Error::Failed(
            "http-call: needs a multi-threaded Tokio runtime".to_string(),
        ));
    }

    let timeout = limits.http_timeout;
    let cap = limits.max_http_body_bytes;
    tokio::task::block_in_place(|| {
        handle.block_on(async move {
            let client = client().https_client.clone();
            match tokio::time::timeout(timeout, client.request(request)).await {
                Err(_) => Err(wit::Error::Failed(format!(
                    "http-call: timed out after {timeout:?}"
                ))),
                Ok(Err(e)) => Err(wit::Error::Failed(format!(
                    "http-call: transport error: {e}"
                ))),
                Ok(Ok(response)) => read_response(response, cap).await,
            }
        })
    })
}

async fn read_response(
    response: http::Response<hyper::body::Incoming>,
    cap: usize,
) -> Result<wit::HttpResponse, wit::Error> {
    let status = response.status();
    let headers = response.headers().clone();
    if let Some(length) = headers
        .get(http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        && length > cap as u64
    {
        return Err(wit::Error::Invalid(format!(
            "http-call: response announces {length} bytes, over the {cap} byte limit"
        )));
    }

    let mut body = Vec::new();
    let mut incoming = response.into_body();
    while let Some(frame) = incoming.frame().await {
        let frame =
            frame.map_err(|e| wit::Error::Failed(format!("http-call: response body: {e}")))?;
        if let Some(data) = frame.data_ref() {
            if body.len() + data.len() > cap {
                return Err(wit::Error::Invalid(format!(
                    "http-call: response body is over the {cap} byte limit"
                )));
            }
            body.extend_from_slice(data);
        }
    }

    Ok(wit::HttpResponse {
        status: status.as_u16(),
        headers: convert::headers_to_wit(&headers),
        body,
    })
}

fn to_header_map(headers: &[wit::Header]) -> Result<HeaderMap, wit::Error> {
    if headers.len() > MAX_HTTP_HEADERS {
        return Err(wit::Error::Invalid(format!(
            "http-call: {} headers, over the {MAX_HTTP_HEADERS} limit",
            headers.len()
        )));
    }
    let mut map = HeaderMap::new();
    let mut total = 0usize;
    for header in headers {
        total += header.name.len() + header.value.len();
        if total > MAX_HTTP_HEADER_BYTES {
            return Err(wit::Error::Invalid(format!(
                "http-call: headers exceed the {MAX_HTTP_HEADER_BYTES} byte limit"
            )));
        }
        if HOP_BY_HOP.contains(&header.name.to_ascii_lowercase().as_str()) {
            continue;
        }
        let name = HeaderName::from_bytes(header.name.as_bytes()).map_err(|e| {
            wit::Error::Invalid(format!(
                "http-call: invalid header name `{}`: {e}",
                header.name
            ))
        })?;
        let value = HeaderValue::from_str(&header.value).map_err(|e| {
            wit::Error::Invalid(format!(
                "http-call: invalid value for header `{}`: {e}",
                header.name
            ))
        })?;
        map.insert(name, value);
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn limits(hosts: &[&str]) -> PluginLimits {
        PluginLimits {
            timeout: std::time::Duration::from_millis(5),
            fuel: 1_000_000,
            max_memory_bytes: 1024 * 1024,
            max_body_bytes: 1024,
            max_output_bytes: 1024,
            http_timeout: std::time::Duration::from_millis(50),
            max_http_body_bytes: 1024,
            http_allowed_hosts: Arc::new(hosts.iter().map(|h| h.to_string()).collect()),
        }
    }

    fn request(url: &str) -> wit::HttpRequest {
        wit::HttpRequest {
            method: "GET".to_string(),
            url: url.to_string(),
            headers: Vec::new(),
            body: None,
        }
    }

    #[test]
    fn refuses_a_host_that_is_not_allowed() {
        let err = call(
            &limits(&["auth.example.com"]),
            request("https://evil.example/x"),
        )
        .unwrap_err();
        assert!(matches!(err, wit::Error::Denied(_)), "{err:?}");
    }

    #[test]
    fn refuses_a_non_http_scheme() {
        let err = call(&limits(&["file"]), request("file:///etc/passwd")).unwrap_err();
        assert!(matches!(err, wit::Error::Invalid(_)), "{err:?}");
    }

    #[test]
    fn allows_the_configured_host_and_port() {
        // The allowlist check passes; the call then fails on the runtime, which
        // is the honest outcome in a unit test with no Tokio runtime.
        let err = call(
            &limits(&["127.0.0.1:9"]),
            request("http://127.0.0.1:9/health"),
        )
        .unwrap_err();
        assert!(matches!(err, wit::Error::Failed(_)), "{err:?}");
    }
}
