//! Spire `ext-authz` plugin: asks an external authorization service about each
//! request and applies its verdict.
//!
//! Flow, all inside `request-filter.on-request-headers`:
//!
//! 1. build a callout (method, headers forwarded from the request, optional
//!    JSON body describing the exchange) and perform it with `host.http-call`;
//! 2. `2xx` — allow, copying the identity headers configured in `copy_headers`
//!    from the authorization response onto the forwarded request;
//! 3. `401`/`403` — deny with the same status and body, preserving
//!    `content-type` and `www-authenticate`;
//! 4. anything else, including a transport failure — `on_error=closed` answers
//!    `503 {"error":"ext-authz unavailable"}`, `on_error=open` lets the request
//!    through and logs a warning.
//!
//! Every callout's outcome is counted, so a failing authorization service is
//! visible as a metric instead of only as traffic.
//!
//! Configuration format (opaque to the gateway): `key=value` lines, e.g.
//!
//! ```text
//! url=https://auth.internal/check
//! method=POST
//! forward_headers=authorization,cookie,x-request-id
//! copy_headers=x-user-id,x-roles
//! on_error=closed
//! body_mode=json
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

use serde_json::{Map, Value};

wit_bindgen::generate!({
    world: "filter-plugin",
    path: "wit",
});

use exports::spire::filter::lifecycle::Guest as Lifecycle;
use exports::spire::filter::request_filter::Guest as RequestFilter;
use exports::spire::filter::response_filter::Guest as ResponseFilter;
use spire::filter::host;
use spire::filter::types;

// Per-instance state, parsed once in `configure`. Kept in a `thread_local`
// because the component model gives each instance its own linear memory but no
// place to hang state; one instance is only ever entered from one thread at a
// time.
thread_local! {
    static CONFIG: RefCell<Config> = RefCell::new(Config::empty());
}

#[derive(Clone)]
struct Config {
    /// Authorization service endpoint. Required.
    url: String,
    /// Upper-case `GET` or `POST`.
    method: String,
    /// Lower-case request header names sent to the authorization service.
    forward_headers: Vec<String>,
    /// Lower-case authorization response header names copied onto the request.
    copy_headers: Vec<String>,
    /// `true` for `on_error=closed`: a failed callout rejects the request.
    fail_closed: bool,
    /// `true` for `body_mode=json`: describe the request in the callout body.
    body_mode_json: bool,
}

impl Config {
    fn empty() -> Config {
        Config {
            url: String::new(),
            method: "POST".to_string(),
            forward_headers: vec!["authorization".to_string()],
            copy_headers: Vec::new(),
            fail_closed: true,
            body_mode_json: true,
        }
    }
}

/// What one callout decided.
enum Verdict {
    /// Forward with these header operations applied.
    Allow(Vec<types::HeaderOp>),
    /// Short-circuit the request with this response.
    Deny(types::LocalReply),
    /// The callout gave no usable answer; `on_error` picks the fallback.
    Unavailable(String),
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
            "url" => parsed.url = value.to_string(),
            "method" => {
                let method = value.to_ascii_uppercase();
                if method != "GET" && method != "POST" {
                    return Err(format!("method must be GET or POST, got `{value}`"));
                }
                parsed.method = method;
            }
            "forward_headers" => parsed.forward_headers = comma_list(value),
            "copy_headers" => parsed.copy_headers = comma_list(value),
            "on_error" => match value.to_ascii_lowercase().as_str() {
                "closed" => parsed.fail_closed = true,
                "open" => parsed.fail_closed = false,
                other => return Err(format!("on_error must be closed or open, got `{other}`")),
            },
            "body_mode" => match value.to_ascii_lowercase().as_str() {
                "json" => parsed.body_mode_json = true,
                "none" => parsed.body_mode_json = false,
                other => return Err(format!("body_mode must be json or none, got `{other}`")),
            },
            _ => {}
        }
    }
    if parsed.url.is_empty() {
        return Err("url is required".to_string());
    }
    Ok(parsed)
}

/// Splits a comma-separated header list into lower-case names, dropping empty
/// entries. `forward_headers=` therefore forwards nothing.
fn comma_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|item| item.trim().to_ascii_lowercase())
        .filter(|item| !item.is_empty())
        .collect()
}

/// First header with `name`, case-insensitively. The host lower-cases names, but
/// an authorization service is arbitrary HTTP and may not.
fn find_header(headers: &[types::Header], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case(name))
        .map(|header| header.value.clone())
}

/// Builds the callout body from the request. `None` with `body_mode=none`.
fn callout_body(
    config: &Config,
    req: &types::Request,
    forwarded: Map<String, Value>,
) -> Option<Vec<u8>> {
    if !config.body_mode_json {
        return None;
    }
    let payload = serde_json::json!({
        "method": req.method,
        "path": req.path,
        "query": req.query,
        "route_id": req.route_id,
        "headers": Value::Object(forwarded),
    });
    // Serialising a `Value` to a string cannot fail: it holds no non-finite
    // float and no custom serializer.
    Some(payload.to_string().into_bytes())
}

/// Performs the callout and classifies its answer.
fn authorize(config: &Config, req: &types::Request) -> Verdict {
    let mut outbound_headers = Vec::new();
    let mut forwarded_json = Map::new();
    for header in &req.headers {
        if config
            .forward_headers
            .iter()
            .any(|wanted| wanted == &header.name)
        {
            forwarded_json.insert(header.name.clone(), Value::String(header.value.clone()));
            outbound_headers.push(header.clone());
        }
    }

    let body = callout_body(config, req, forwarded_json);
    // A JSON callout needs its content type: the service on the other side
    // cannot be expected to guess, and the plugin never forwards the client's
    // own content type.
    if body.is_some()
        && !outbound_headers
            .iter()
            .any(|header| header.name.eq_ignore_ascii_case("content-type"))
    {
        outbound_headers.push(types::Header {
            name: "content-type".to_string(),
            value: "application/json".to_string(),
        });
    }

    let request = types::HttpRequest {
        method: config.method.clone(),
        url: config.url.clone(),
        headers: outbound_headers,
        body,
    };
    let resp = match host::http_call(&request) {
        Ok(resp) => resp,
        Err(err) => return Verdict::Unavailable(format!("callout failed: {err:?}")),
    };

    if (200..300).contains(&resp.status) {
        let mut ops = Vec::new();
        for name in &config.copy_headers {
            if let Some(value) = find_header(&resp.headers, name) {
                ops.push(types::HeaderOp::Set(types::Header {
                    name: name.clone(),
                    value,
                }));
            }
        }
        return Verdict::Allow(ops);
    }

    if resp.status == 401 || resp.status == 403 {
        let mut headers = vec![types::Header {
            name: "content-type".to_string(),
            value: find_header(&resp.headers, "content-type")
                .unwrap_or_else(|| "application/json".to_string()),
        }];
        if let Some(challenge) = find_header(&resp.headers, "www-authenticate") {
            headers.push(types::Header {
                name: "www-authenticate".to_string(),
                value: challenge,
            });
        }
        return Verdict::Deny(types::LocalReply {
            status: resp.status,
            headers,
            body: resp.body,
        });
    }

    Verdict::Unavailable(format!("authorization service returned {}", resp.status))
}

/// The `503` used by `on_error=closed`.
fn unavailable_reply() -> types::LocalReply {
    types::LocalReply {
        status: 503,
        headers: vec![types::Header {
            name: "content-type".to_string(),
            value: "application/json".to_string(),
        }],
        body: br#"{"error":"ext-authz unavailable"}"#.to_vec(),
    }
}

/// Counts one callout outcome: `allow`, `deny`, or `error`.
fn record_outcome(outcome: &str) {
    if let Err(err) = host::incr_counter(
        "requests",
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
                "ext-authz {} starting on api {} with {:?}",
                info.version, info.api_version, info.capabilities
            ),
        );
        Ok(())
    }

    fn configure(config: String) -> Result<(), types::Error> {
        // `configure` runs once per distinct configuration, not per request:
        // this is where parsing belongs.
        let parsed = parse(&config).map_err(types::Error::Invalid)?;
        host::log(
            types::LogLevel::Debug,
            &format!(
                "callout {} {} forwarding {:?}, copying {:?}, on_error={}",
                parsed.method,
                parsed.url,
                parsed.forward_headers,
                parsed.copy_headers,
                if parsed.fail_closed { "closed" } else { "open" }
            ),
        );
        CONFIG.with(|config| *config.borrow_mut() = parsed);
        Ok(())
    }

    fn stop() {
        // Nothing buffered: every callout is request/response.
    }
}

impl RequestFilter for Component {
    fn on_request_headers(req: types::Request) -> types::HeadersDecision {
        CONFIG.with(|cell| {
            let config = cell.borrow();
            match authorize(&config, &req) {
                Verdict::Allow(ops) => {
                    record_outcome("allow");
                    types::HeadersDecision::Proceed(ops)
                }
                Verdict::Deny(reply) => {
                    record_outcome("deny");
                    types::HeadersDecision::Reply(reply)
                }
                Verdict::Unavailable(reason) => {
                    record_outcome("error");
                    host::log(
                        types::LogLevel::Warn,
                        &format!("authorization callout unavailable: {reason}"),
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
        // The callout only looks at headers, so bodies pass through untouched.
        types::BodyDecision::Proceed(chunk.bytes)
    }

    fn on_request_trailers(trailers: Vec<types::Header>) -> types::HeadersDecision {
        let _ = trailers;
        types::HeadersDecision::Proceed(Vec::new())
    }
}

impl ResponseFilter for Component {
    fn on_response_headers(resp: types::Response) -> types::HeadersDecision {
        // Authorization is a request-phase concern; the upstream response is
        // forwarded unchanged.
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
