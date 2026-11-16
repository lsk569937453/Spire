//! Spire `oauth2-client` plugin: gives upstream requests a client-credentials
//! access token.
//!
//! Flow, all inside `request-filter.on-request-headers`:
//!
//! 1. if the configured header already carries a value, the request is left
//!    alone (the caller owns its own credentials);
//! 2. otherwise a cached token is read from shared data — process-wide, so
//!    every worker thread and every instance reuses one fetch;
//! 3. when the cache is missing or expired, the token endpoint is called with
//!    `grant_type=client_credentials`, the token is cached until shortly before
//!    it expires, and the header is injected.
//!
//! Failures (`on_error=closed`) answer `503 {"error":"oauth2 token
//! unavailable"}` instead of forwarding a request that cannot be authorized.
//!
//! Configuration format (opaque to the gateway): `key=value` lines, e.g.
//!
//! ```text
//! token_url=https://idp.internal/oauth/token
//! client_id=spire
//! client_secret=s3cret
//! scope=api.read api.write
//! header=authorization
//! on_error=closed
//! cache_seconds=0
//! ```
//!
//! Build:
//!
//! ```sh
//! rustup target add wasm32-wasip2
//! cargo build --release --target wasm32-wasip2
//! # dist/plugin.wasm is the component the gateway loads
//! ```

use std::cell::RefCell;

use serde_json::Value;

wit_bindgen::generate!({
    world: "filter-plugin",
    path: "wit",
});

use exports::spire::filter::lifecycle::Guest as Lifecycle;
use exports::spire::filter::request_filter::Guest as RequestFilter;
use exports::spire::filter::response_filter::Guest as ResponseFilter;
use spire::filter::host;
use spire::filter::types;

/// Shared-data namespace. The key is the token URL, so two routes that use the
/// same identity provider share one token and two different ones do not.
const NAMESPACE: &str = "oauth2-client";

/// How much of the token's lifetime is left on the table so a token is never
/// injected on the edge of expiry.
const EXPIRY_MARGIN_SECONDS: u64 = 30;

/// Never cache for less than this, whatever the endpoint said.
const MIN_CACHE_SECONDS: u64 = 1;

// Per-instance state, parsed once in `configure`. Kept in a `thread_local`
// because the component model gives each instance its own linear memory but no
// place to hang state; one instance is only ever entered from one thread at a
// time.
thread_local! {
    static CONFIG: RefCell<Config> = RefCell::new(Config::empty());
}

struct Config {
    /// Token endpoint. Required; also the shared-data cache key.
    token_url: String,
    client_id: String,
    client_secret: String,
    /// Optional space-separated scopes.
    scope: String,
    /// Lower-case request header the bearer token is written to.
    header: String,
    /// `true` for `on_error=closed`: a failed fetch rejects the request.
    fail_closed: bool,
    /// `0` means honour the endpoint's `expires_in`.
    cache_seconds: u64,
}

impl Config {
    fn empty() -> Config {
        Config {
            token_url: String::new(),
            client_id: String::new(),
            client_secret: String::new(),
            scope: String::new(),
            header: "authorization".to_string(),
            fail_closed: true,
            cache_seconds: 0,
        }
    }
}

/// Parses `key=value` lines. Unknown keys are ignored, which is what lets a
/// newer gateway pass new keys to an older plugin.
fn parse(config: &str) -> Result<Config, String> {
    let mut parsed = Config::empty();
    for line in config.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "token_url" => parsed.token_url = value.to_string(),
            "client_id" => parsed.client_id = value.to_string(),
            "client_secret" => parsed.client_secret = value.to_string(),
            "scope" => parsed.scope = value.to_string(),
            "header" => {
                let header = value.to_ascii_lowercase();
                if header.is_empty() {
                    return Err("header must not be empty".to_string());
                }
                parsed.header = header;
            }
            "on_error" => match value.to_ascii_lowercase().as_str() {
                "closed" => parsed.fail_closed = true,
                "open" => parsed.fail_closed = false,
                other => return Err(format!("on_error must be closed or open, got `{other}`")),
            },
            "cache_seconds" => {
                parsed.cache_seconds = value
                    .parse::<u64>()
                    .map_err(|_| format!("cache_seconds must be a number, got `{value}`"))?;
            }
            _ => {}
        }
    }
    if parsed.token_url.is_empty() {
        return Err("token_url is required".to_string());
    }
    if parsed.client_id.is_empty() {
        return Err("client_id is required".to_string());
    }
    if parsed.client_secret.is_empty() {
        return Err("client_secret is required".to_string());
    }
    Ok(parsed)
}

/// Form-encodes one value: everything outside RFC 3986's unreserved set becomes
/// `%XX`, enough for `application/x-www-form-urlencoded` in practice.
fn form_escape(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            other => {
                out.push('%');
                out.push(HEX[usize::from(other >> 4)] as char);
                out.push(HEX[usize::from(other & 0x0f)] as char);
            }
        }
    }
    out
}

/// The `application/x-www-form-urlencoded` body of the token request.
fn form_body(config: &Config) -> Vec<u8> {
    let mut body = format!(
        "grant_type=client_credentials&client_id={}&client_secret={}",
        form_escape(&config.client_id),
        form_escape(&config.client_secret)
    );
    if !config.scope.is_empty() {
        body.push_str("&scope=");
        body.push_str(&form_escape(&config.scope));
    }
    body.into_bytes()
}

/// Cached token, when the entry exists and has not expired. The entry is eight
/// little-endian bytes of expiry in milliseconds followed by the token itself.
fn cached_token(config: &Config) -> Option<String> {
    let entry = host::get_shared_data(NAMESPACE, &config.token_url)?;
    let deadline = u64::from_le_bytes(*entry.first_chunk::<8>()?);
    let token = entry.get(8..)?;
    if token.is_empty() || host::now_millis() >= deadline {
        return None;
    }
    std::str::from_utf8(token).ok().map(str::to_string)
}

/// Writes the cache entry. A failure here only costs a refetch, so it is logged
/// and the token injection continues.
fn store_token(config: &Config, token: &str, ttl_seconds: u64) {
    let ttl_millis = ttl_seconds.max(MIN_CACHE_SECONDS).saturating_mul(1000);
    let deadline = host::now_millis().saturating_add(ttl_millis);
    let mut entry = deadline.to_le_bytes().to_vec();
    entry.extend_from_slice(token.as_bytes());
    if let Err(err) = host::set_shared_data(NAMESPACE, &config.token_url, &entry) {
        host::log(
            types::LogLevel::Warn,
            &format!("cannot cache token: {err:?}"),
        );
    }
}

/// Calls the token endpoint and caches the result. The error is a description
/// safe to log: it never contains the client secret.
fn fetch_token(config: &Config) -> Result<String, String> {
    let request = types::HttpRequest {
        method: "POST".to_string(),
        url: config.token_url.clone(),
        headers: vec![types::Header {
            name: "content-type".to_string(),
            value: "application/x-www-form-urlencoded".to_string(),
        }],
        body: Some(form_body(config)),
    };
    let resp = match host::http_call(&request) {
        Ok(resp) => resp,
        Err(err) => return Err(format!("token request failed: {err:?}")),
    };
    if !(200..300).contains(&resp.status) {
        return Err(format!("token endpoint returned {}", resp.status));
    }
    let payload: Value = serde_json::from_slice(&resp.body)
        .map_err(|err| format!("token response is not JSON: {err}"))?;
    let token = payload
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "token response has no access_token".to_string())?
        .to_string();
    let expires_in = payload
        .get("expires_in")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let ttl_seconds = if config.cache_seconds > 0 {
        config.cache_seconds
    } else {
        expires_in.saturating_sub(EXPIRY_MARGIN_SECONDS)
    };
    store_token(config, &token, ttl_seconds);
    Ok(token)
}

/// `Set` the configured header to `<scheme> <token>`.
fn inject(config: &Config, token: &str) -> types::HeadersDecision {
    types::HeadersDecision::Proceed(vec![types::HeaderOp::Set(types::Header {
        name: config.header.clone(),
        value: format!("Bearer {token}"),
    })])
}

/// The `503` used by `on_error=closed`.
fn unavailable_reply() -> types::LocalReply {
    types::LocalReply {
        status: 503,
        headers: vec![types::Header {
            name: "content-type".to_string(),
            value: "application/json".to_string(),
        }],
        body: br#"{"error":"oauth2 token unavailable"}"#.to_vec(),
    }
}

/// Counts one token fetch: `ok` or `error`. Cache hits are not counted, so the
/// metric measures calls to the identity provider.
fn record_fetch(outcome: &str) {
    if let Err(err) = host::incr_counter(
        "token_fetches",
        &[("outcome".to_string(), outcome.to_string())],
        1,
    ) {
        host::log(
            types::LogLevel::Warn,
            &format!("cannot record metric: {err:?}"),
        );
    }
}

struct Component;

impl Lifecycle for Component {
    fn start(info: types::PluginInfo) -> Result<(), types::Error> {
        host::log(
            types::LogLevel::Info,
            &format!(
                "oauth2-client {} starting on api {} with {:?}",
                info.version, info.api_version, info.capabilities
            ),
        );
        Ok(())
    }

    fn configure(config: String) -> Result<(), types::Error> {
        // `configure` runs once per distinct configuration, not per request:
        // this is where parsing belongs. The client secret is never logged.
        let parsed = parse(&config).map_err(types::Error::Invalid)?;
        host::log(
            types::LogLevel::Debug,
            &format!(
                "token endpoint {}, header {}, on_error={}, cache_seconds={}",
                parsed.token_url,
                parsed.header,
                if parsed.fail_closed { "closed" } else { "open" },
                parsed.cache_seconds
            ),
        );
        CONFIG.with(|config| *config.borrow_mut() = parsed);
        Ok(())
    }

    fn stop() {
        // The token cache lives in shared data, so nothing is flushed here.
    }
}

impl RequestFilter for Component {
    fn on_request_headers(req: types::Request) -> types::HeadersDecision {
        CONFIG.with(|cell| {
            let config = cell.borrow();
            // The caller already sent credentials: never overwrite them.
            if req
                .headers
                .iter()
                .any(|header| header.name.eq_ignore_ascii_case(&config.header))
            {
                return types::HeadersDecision::Proceed(Vec::new());
            }
            if let Some(token) = cached_token(&config) {
                return inject(&config, &token);
            }
            match fetch_token(&config) {
                Ok(token) => {
                    record_fetch("ok");
                    inject(&config, &token)
                }
                Err(reason) => {
                    record_fetch("error");
                    host::log(
                        types::LogLevel::Warn,
                        &format!("oauth2 token unavailable: {reason}"),
                    );
                    if config.fail_closed {
                        types::HeadersDecision::Reply(unavailable_reply())
                    } else {
                        types::HeadersDecision::Proceed(Vec::new())
                    }
                }
            }
        })
    }

    fn on_request_body(chunk: types::BodyChunk) -> types::BodyDecision {
        // Tokens are a header concern; bodies pass through untouched.
        types::BodyDecision::Proceed(chunk.bytes)
    }

    fn on_request_trailers(trailers: Vec<types::Header>) -> types::HeadersDecision {
        let _ = trailers;
        types::HeadersDecision::Proceed(Vec::new())
    }
}

impl ResponseFilter for Component {
    fn on_response_headers(resp: types::Response) -> types::HeadersDecision {
        let _ = resp;
        types::HeadersDecision::Proceed(Vec::new())
    }

    fn on_response_body(chunk: types::BodyChunk) -> types::BodyDecision {
        types::BodyDecision::Proceed(chunk.bytes)
    }

    fn on_response_trailers(trailers: Vec<types::Header>) -> types::HeadersDecision {
        let _ = trailers;
        types::HeadersDecision::Proceed(Vec::new())
    }

    fn on_log(summary: types::StreamSummary) {
        let _ = summary;
    }
}

export!(Component);
