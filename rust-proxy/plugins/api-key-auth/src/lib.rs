//! API-key authentication.
//!
//! Reads one configured request header and compares its value against the
//! accepted key list in constant time. A missing or wrong key is answered with
//! a local `401`, so the request never reaches the upstream.
//!
//! Configuration is `key=value` lines (opaque to the gateway), e.g.
//!
//! ```text
//! header=x-api-key
//! keys=k1,k2
//! strip=false
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

const DEFAULT_HEADER: &str = "x-api-key";
const ERROR_BODY: &[u8] = br#"{"error":"invalid api key"}"#;

thread_local! {
    static CONFIG: RefCell<Config> = RefCell::new(Config::defaults());
}

#[derive(Clone)]
struct Config {
    /// Header name, always lowercased because the host lowercases names too.
    header: String,
    keys: Vec<String>,
    strip: bool,
}

impl Config {
    fn defaults() -> Config {
        Config {
            header: DEFAULT_HEADER.to_string(),
            keys: Vec::new(),
            strip: false,
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
            "header" => {
                let name = value.trim().to_ascii_lowercase();
                if !name.is_empty() {
                    parsed.header = name;
                }
            }
            "keys" => parsed.keys = parse_keys(value),
            "strip" => parsed.strip = parse_bool(value.trim()),
            _ => {}
        }
    }
    parsed
}

fn parse_keys(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_string)
        .collect()
}

fn parse_bool(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "true" | "1" | "yes" | "on"
    )
}

/// Compares two byte strings without an early exit: every position is folded
/// into one accumulator, so the running time depends on the length and not on
/// where the first mismatch is.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut diff = u8::from(left.len() != right.len());
    let len = left.len().max(right.len());
    for index in 0..len {
        let left_byte = left.get(index).copied().unwrap_or(0);
        let right_byte = right.get(index).copied().unwrap_or(0);
        diff |= left_byte ^ right_byte;
    }
    diff == 0
}

fn header_value(headers: &[types::Header], name: &str) -> String {
    headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case(name))
        .map(|header| header.value.clone())
        .unwrap_or_default()
}

fn reject() -> types::HeadersDecision {
    types::HeadersDecision::Reply(types::LocalReply {
        status: 401,
        headers: vec![types::Header {
            name: "content-type".to_string(),
            value: "application/json".to_string(),
        }],
        body: ERROR_BODY.to_vec(),
    })
}

struct Component;

impl Lifecycle for Component {
    fn start(info: types::PluginInfo) -> Result<(), types::Error> {
        host::log(
            types::LogLevel::Info,
            &format!(
                "api-key-auth {} starting on api {}",
                info.version, info.api_version
            ),
        );
        Ok(())
    }

    fn configure(config: String) -> Result<(), types::Error> {
        // `configure` runs once per distinct configuration, not per request.
        let parsed = parse(&config);
        if parsed.keys.is_empty() {
            return Err(types::Error::Invalid(
                "keys is required and must list at least one API key".into(),
            ));
        }
        host::log(
            types::LogLevel::Info,
            &format!(
                "api-key-auth: checking header '{}' against {} key(s), strip={}",
                parsed.header,
                parsed.keys.len(),
                parsed.strip
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
            let provided = header_value(&req.headers, &config.header);
            let mut matched = false;
            for key in &config.keys {
                matched |= constant_time_eq(provided.as_bytes(), key.as_bytes());
            }
            if !matched {
                host::log(
                    types::LogLevel::Debug,
                    "api-key-auth: missing or invalid api key",
                );
                return reject();
            }
            if config.strip {
                types::HeadersDecision::Proceed(vec![types::HeaderOp::Remove(
                    config.header.clone(),
                )])
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
