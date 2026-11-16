//! External rate limiting for Spire.
//!
//! On every request the plugin asks an HTTP limiter service for a decision and
//! turns the answer into a gateway decision:
//!
//! * allowed  -> the request is forwarded untouched;
//! * denied   -> a local `429` with `retry-after` is sent instead;
//! * anything else (non-2xx, unparseable body, `http-call` error) -> the
//!   configured `on_error` policy decides between a local `503` and forwarding.
//!
//! Configuration is `key=value` lines, parsed exactly once in `configure`:
//!
//! ```text
//! url=https://limiter.internal/check   # required, absolute http(s) URL
//! key=ip                               # ip | header | route (default ip)
//! header=x-api-key                     # required when key=header
//! on_error=closed                      # closed | open (default closed)
//! ```
//!
//! The outbound call happens inside `on-request-headers`, so the route must
//! grant `host.http-call` and list the limiter host in `http_allowed_hosts`.
//!
//! Build:
//!
//! ```sh
//! rustup target add wasm32-wasip2
//! cargo build --release --target wasm32-wasip2
//! # dist/plugin.wasm is the component the gateway loads
//! ```

use std::cell::RefCell;

wit_bindgen::generate!({
    world: "filter-plugin",
    path: "wit",
});

use exports::spire::filter::lifecycle::Guest as Lifecycle;
use exports::spire::filter::request_filter::Guest as RequestFilter;
use exports::spire::filter::response_filter::Guest as ResponseFilter;
use spire::filter::host;
use spire::filter::types;
use types::Header;

// Per-instance state. Kept in a `thread_local` because the component model
// gives each instance its own linear memory but not a place to hang state;
// one instance is only ever entered from one thread at a time.
thread_local! {
    static CONFIG: RefCell<Option<Config>> = const { RefCell::new(None) };
}

/// Body of a local `429` reply.
const DENIED_BODY: &[u8] = br#"{"error":"rate limited"}"#;
/// Body of a local `503` reply, used when the limiter cannot give an answer.
const UNAVAILABLE_BODY: &[u8] = br#"{"error":"rate limiter unavailable"}"#;
/// `retry-after` value when the limiter denied without saying for how long.
const DEFAULT_RETRY_AFTER_SECS: u64 = 1;

/// Which value identifies the client to the limiter.
#[derive(Clone, Debug, PartialEq, Eq)]
enum KeyMode {
    /// The resolved client IP (already the trusted-proxy-aware address).
    Ip,
    /// A configured request header, lowercased.
    Header(String),
    /// The matched `route_id`.
    Route,
}

/// What to do when the limiter cannot answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OnError {
    /// Refuse the request: never let traffic through a limiter outage.
    Closed,
    /// Forward the request and log a warning.
    Open,
}

/// Parsed once per distinct `config:` blob.
#[derive(Clone, Debug)]
struct Config {
    url: String,
    key: KeyMode,
    on_error: OnError,
}

/// Parses the `key=value` blob. Unknown keys are rejected rather than ignored:
/// a typo in `on_error` or `key` must not silently weaken the limiter.
fn parse(config: &str) -> Result<Config, types::Error> {
    let mut url: Option<String> = None;
    let mut key_mode = "ip";
    let mut header: Option<String> = None;
    let mut on_error = OnError::Closed;

    for line in config.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((raw_name, raw_value)) = line.split_once('=') else {
            return Err(types::Error::Invalid(format!(
                "`{line}` is not a key=value line"
            )));
        };
        let (name, value) = (raw_name.trim(), raw_value.trim());
        match name {
            "url" => {
                if value.is_empty() {
                    return Err(types::Error::Invalid("url must not be empty".to_string()));
                }
                url = Some(value.to_string());
            }
            "key" => {
                key_mode = match value.to_ascii_lowercase().as_str() {
                    "ip" => "ip",
                    "header" => "header",
                    "route" => "route",
                    other => {
                        return Err(types::Error::Invalid(format!(
                            "key must be ip, header or route, not `{other}`"
                        )))
                    }
                };
            }
            "header" => {
                if value.is_empty() {
                    return Err(types::Error::Invalid(
                        "header must not be empty when set".to_string(),
                    ));
                }
                header = Some(value.to_ascii_lowercase());
            }
            "on_error" => {
                on_error = match value.to_ascii_lowercase().as_str() {
                    "closed" => OnError::Closed,
                    "open" => OnError::Open,
                    other => {
                        return Err(types::Error::Invalid(format!(
                            "on_error must be closed or open, not `{other}`"
                        )))
                    }
                };
            }
            other => {
                return Err(types::Error::Invalid(format!("unknown key `{other}`")));
            }
        }
    }

    let url = url.ok_or_else(|| types::Error::Invalid("url is required".to_string()))?;
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(types::Error::Invalid(format!(
            "url must be an absolute http(s) URL, got `{url}`"
        )));
    }
    let key = match key_mode {
        "header" => KeyMode::Header(header.ok_or_else(|| {
            types::Error::Invalid("header is required when key=header".to_string())
        })?),
        "route" => KeyMode::Route,
        _ => KeyMode::Ip,
    };

    Ok(Config { url, key, on_error })
}

/// The value the limiter buckets on. A missing configured header yields an
/// empty key on purpose: the limiter still answers, so an unidentifiable
/// client is not silently allowed through.
fn key_value(config: &Config, req: &types::Request) -> String {
    match &config.key {
        KeyMode::Ip => req.client_ip.clone(),
        KeyMode::Route => req.route_id.clone(),
        KeyMode::Header(name) => header_value(&req.headers, name).unwrap_or_default(),
    }
}

fn header_value(headers: &[types::Header], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case(name))
        .map(|header| header.value.clone())
}

/// `retry_after` from the limiter body, as a number or a numeric string.
fn retry_after(value: &serde_json::Value) -> u64 {
    match value.get("retry_after") {
        Some(serde_json::Value::Number(number)) => {
            number.as_u64().unwrap_or(DEFAULT_RETRY_AFTER_SECS)
        }
        Some(serde_json::Value::String(text)) => {
            text.trim().parse().unwrap_or(DEFAULT_RETRY_AFTER_SECS)
        }
        _ => DEFAULT_RETRY_AFTER_SECS,
    }
}

fn record(outcome: &str) {
    if let Err(e) = host::incr_counter(
        "requests",
        &[("outcome".to_string(), outcome.to_string())],
        1,
    ) {
        host::log(
            types::LogLevel::Warn,
            &format!("cannot record metric: {e:?}"),
        );
    }
}

/// Builds a local JSON reply. `extra` lets the `429` carry `retry-after`.
fn json_reply(status: u16, extra: Vec<Header>, body: &[u8]) -> types::HeadersDecision {
    let mut headers = vec![Header {
        name: "content-type".to_string(),
        value: "application/json".to_string(),
    }];
    headers.extend(extra);
    types::HeadersDecision::Reply(types::LocalReply {
        status,
        headers,
        body: body.to_vec(),
    })
}

/// Applies the `on_error` policy after the limiter could not answer.
fn unavailable(config: &Config, reason: &str) -> types::HeadersDecision {
    record("error");
    match config.on_error {
        OnError::Closed => {
            host::log(
                types::LogLevel::Warn,
                &format!("rate limiter unavailable, refusing request: {reason}"),
            );
            json_reply(503, Vec::new(), UNAVAILABLE_BODY)
        }
        OnError::Open => {
            host::log(
                types::LogLevel::Warn,
                &format!("rate limiter unavailable, allowing request: {reason}"),
            );
            types::HeadersDecision::Proceed(Vec::new())
        }
    }
}

/// Asks the limiter and turns its answer into a decision.
fn evaluate(config: &Config, req: &types::Request) -> types::HeadersDecision {
    let payload = serde_json::json!({
        "key": key_value(config, req),
        "route_id": req.route_id,
        "method": req.method,
        "path": req.path,
    });
    let body = match serde_json::to_vec(&payload) {
        Ok(body) => body,
        Err(e) => return unavailable(config, &format!("cannot encode check request: {e}")),
    };

    let request = types::HttpRequest {
        method: "POST".to_string(),
        url: config.url.clone(),
        headers: vec![
            Header {
                name: "content-type".to_string(),
                value: "application/json".to_string(),
            },
            Header {
                name: "accept".to_string(),
                value: "application/json".to_string(),
            },
        ],
        body: Some(body),
    };

    let response = match host::http_call(&request) {
        Ok(response) => response,
        Err(e) => return unavailable(config, &format!("http-call failed: {e:?}")),
    };
    if !(200..300).contains(&response.status) {
        return unavailable(config, &format!("limiter answered {}", response.status));
    }
    let value: serde_json::Value = match serde_json::from_slice(&response.body) {
        Ok(value) => value,
        Err(e) => return unavailable(config, &format!("cannot parse limiter body: {e}")),
    };

    match value.get("allowed").and_then(|allowed| allowed.as_bool()) {
        Some(true) => {
            record("allow");
            types::HeadersDecision::Proceed(Vec::new())
        }
        Some(false) => {
            let retry_after = retry_after(&value);
            record("deny");
            json_reply(
                429,
                vec![Header {
                    name: "retry-after".to_string(),
                    value: retry_after.to_string(),
                }],
                DENIED_BODY,
            )
        }
        None => unavailable(config, "limiter body has no boolean `allowed`"),
    }
}

struct Component;

impl Lifecycle for Component {
    fn start(info: types::PluginInfo) -> Result<(), types::Error> {
        host::log(
            types::LogLevel::Info,
            &format!(
                "ratelimit-external {} starting on api {} with {:?}",
                info.version, info.api_version, info.capabilities
            ),
        );
        Ok(())
    }

    fn configure(config: String) -> Result<(), types::Error> {
        let parsed = parse(&config)?;
        host::log(
            types::LogLevel::Debug,
            &format!(
                "checking {} for key mode {:?}, on_error {:?}",
                parsed.url, parsed.key, parsed.on_error
            ),
        );
        CONFIG.with(|cell| *cell.borrow_mut() = Some(parsed));
        Ok(())
    }

    fn stop() {
        // Nothing buffered: every check is synchronous.
    }
}

impl RequestFilter for Component {
    fn on_request_headers(req: types::Request) -> types::HeadersDecision {
        // The borrow is held across the host call: a host call never reenters
        // this instance, so the `RefCell` cannot be borrowed twice.
        CONFIG.with(|cell| {
            let guard = cell.borrow();
            match guard.as_ref() {
                Some(config) => evaluate(config, &req),
                None => {
                    // `configure` always runs first; treat a missing config as
                    // an error instead of guessing.
                    record("error");
                    json_reply(503, Vec::new(), UNAVAILABLE_BODY)
                }
            }
        })
    }

    fn on_request_body(chunk: types::BodyChunk) -> types::BodyDecision {
        // The limiter only needs headers; the route never requests a body.
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
        // The metric is recorded at decision time, where the limiter's verdict
        // is known; the summary only helps operator debugging.
        host::log(
            types::LogLevel::Debug,
            &format!(
                "{} {} -> {} in {}ms",
                summary.method, summary.path, summary.status, summary.total_millis
            ),
        );
    }
}

export!(Component);
