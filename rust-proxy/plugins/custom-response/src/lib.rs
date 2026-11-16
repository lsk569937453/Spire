//! Custom response.
//!
//! Answers a request with a fixed local response whenever every configured
//! condition holds. With no conditions configured the plugin always replies.
//!
//! Configuration is `key=value` lines (opaque to the gateway). The `body` value
//! is everything after the first `=`:
//!
//! ```text
//! status=403
//! content_type=application/json
//! body={"error":"blocked"}
//! path_prefix=/admin
//! header=x-blocked
//! value=1
//! ```
//!
//! Build:
//!
//! ```sh
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

const DEFAULT_STATUS: u16 = 403;
const DEFAULT_CONTENT_TYPE: &str = "application/json";
const DEFAULT_BODY: &str = r#"{"error":"blocked"}"#;

thread_local! {
    static CONFIG: RefCell<Config> = RefCell::new(Config::defaults());
}

#[derive(Clone)]
struct Config {
    status: u16,
    content_type: String,
    body: String,
    /// Lowercased header name for the optional header condition.
    header: Option<String>,
    /// Required value for the optional header condition.
    value: Option<String>,
    path_prefix: Option<String>,
}

impl Config {
    fn defaults() -> Config {
        Config {
            status: DEFAULT_STATUS,
            content_type: DEFAULT_CONTENT_TYPE.to_string(),
            body: DEFAULT_BODY.to_string(),
            header: None,
            value: None,
            path_prefix: None,
        }
    }
}

/// Parses `key=value` lines. Unknown keys are ignored so a newer gateway can
/// pass extra keys to an older plugin.
fn parse(config: &str) -> Config {
    let mut parsed = Config::defaults();
    for line in config.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "status" => {
                if let Ok(status) = value.trim().parse::<u16>() {
                    parsed.status = status;
                }
            }
            "content_type" => {
                let content_type = value.trim();
                if !content_type.is_empty() {
                    parsed.content_type = content_type.to_string();
                }
            }
            // Everything after the first '=' belongs to the body.
            "body" => parsed.body = value.trim().to_string(),
            "header" => {
                let name = value.trim().to_ascii_lowercase();
                if !name.is_empty() {
                    parsed.header = Some(name);
                }
            }
            "value" => parsed.value = Some(value.trim().to_string()),
            "path_prefix" => {
                let prefix = value.trim();
                if !prefix.is_empty() {
                    parsed.path_prefix = Some(prefix.to_string());
                }
            }
            _ => {}
        }
    }
    parsed
}

/// True when every configured condition holds for this request.
fn matches(config: &Config, req: &types::Request) -> bool {
    if let Some(prefix) = &config.path_prefix {
        if !req.path.starts_with(prefix.as_str()) {
            return false;
        }
    }
    if let Some(name) = &config.header {
        let Some(header) = req
            .headers
            .iter()
            .find(|header| header.name.eq_ignore_ascii_case(name))
        else {
            return false;
        };
        if let Some(expected) = &config.value {
            if header.value != *expected {
                return false;
            }
        }
    }
    true
}

fn reply(config: &Config) -> types::HeadersDecision {
    types::HeadersDecision::Reply(types::LocalReply {
        status: config.status,
        headers: vec![types::Header {
            name: "content-type".to_string(),
            value: config.content_type.clone(),
        }],
        body: config.body.clone().into_bytes(),
    })
}

struct Component;

impl Lifecycle for Component {
    fn start(info: types::PluginInfo) -> Result<(), types::Error> {
        host::log(
            types::LogLevel::Info,
            &format!(
                "custom-response {} starting on api {}",
                info.version, info.api_version
            ),
        );
        Ok(())
    }

    fn configure(config: String) -> Result<(), types::Error> {
        // `configure` runs once per distinct configuration, not per request.
        let parsed = parse(&config);
        host::log(
            types::LogLevel::Info,
            &format!(
                "custom-response: {} {} body {} byte(s), path_prefix={} header={} value={}",
                parsed.status,
                parsed.content_type,
                parsed.body.len(),
                parsed.path_prefix.as_deref().unwrap_or("-"),
                parsed.header.as_deref().unwrap_or("-"),
                parsed.value.as_deref().unwrap_or("-")
            ),
        );
        CONFIG.with(|cell| *cell.borrow_mut() = parsed);
        Ok(())
    }

    fn stop() {
        // Nothing buffered.
    }
}

impl RequestFilter for Component {
    fn on_request_headers(req: types::Request) -> types::HeadersDecision {
        CONFIG.with(|cell| {
            let config = cell.borrow();
            if matches(&config, &req) {
                reply(&config)
            } else {
                types::HeadersDecision::Proceed(Vec::new())
            }
        })
    }

    fn on_request_body(chunk: types::BodyChunk) -> types::BodyDecision {
        // Unchanged pass-through; body filtering is not enabled on this route.
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
