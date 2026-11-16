//! Fixed-window local rate limiter.
//!
//! Counts requests per key inside a window using the plugin shared-data store,
//! so the counter survives across exchanges and worker threads. The state per
//! key is 16 bytes: a `u64` little-endian count followed by a `u64`
//! little-endian window start in milliseconds since the UNIX epoch.
//!
//! Configuration is `key=value` lines (opaque to the gateway), e.g.
//!
//! ```text
//! key=ip
//! header=
//! limit=100
//! window_seconds=60
//! status=429
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

const NAMESPACE: &str = "local-ratelimit";
const STATE_LEN: usize = 16;
const DEFAULT_LIMIT: u64 = 100;
const DEFAULT_WINDOW_SECONDS: u64 = 60;
const DEFAULT_STATUS: u16 = 429;
const ERROR_BODY: &[u8] = br#"{"error":"rate limited"}"#;

thread_local! {
    static CONFIG: RefCell<Config> = RefCell::new(Config::defaults());
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum KeyPolicy {
    Ip,
    Header,
    Route,
}

impl KeyPolicy {
    fn as_str(self) -> &'static str {
        match self {
            KeyPolicy::Ip => "ip",
            KeyPolicy::Header => "header",
            KeyPolicy::Route => "route",
        }
    }

    fn parse(value: &str) -> Option<KeyPolicy> {
        match value {
            "ip" => Some(KeyPolicy::Ip),
            "header" => Some(KeyPolicy::Header),
            "route" => Some(KeyPolicy::Route),
            _ => None,
        }
    }
}

#[derive(Clone)]
struct Config {
    key: KeyPolicy,
    /// Lowercased header name used when `key=header`.
    header: String,
    limit: u64,
    window_seconds: u64,
    status: u16,
}

impl Config {
    fn defaults() -> Config {
        Config {
            key: KeyPolicy::Ip,
            header: String::new(),
            limit: DEFAULT_LIMIT,
            window_seconds: DEFAULT_WINDOW_SECONDS,
            status: DEFAULT_STATUS,
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
            "key" => match KeyPolicy::parse(value.trim()) {
                Some(policy) => parsed.key = policy,
                None => host::log(
                    types::LogLevel::Warn,
                    &format!(
                        "local-ratelimit: unknown key policy '{}', keeping '{}'",
                        value.trim(),
                        parsed.key.as_str()
                    ),
                ),
            },
            "header" => {
                let name = value.trim().to_ascii_lowercase();
                if !name.is_empty() {
                    parsed.header = name;
                }
            }
            "limit" => {
                if let Ok(limit) = value.trim().parse::<u64>() {
                    parsed.limit = limit;
                }
            }
            "window_seconds" => {
                if let Ok(seconds) = value.trim().parse::<u64>() {
                    parsed.window_seconds = seconds;
                }
            }
            "status" => {
                if let Ok(status) = value.trim().parse::<u16>() {
                    parsed.status = status;
                }
            }
            _ => {}
        }
    }
    parsed
}

fn window_millis(seconds: u64) -> u64 {
    seconds.saturating_mul(1000).max(1)
}

fn header_value(headers: &[types::Header], name: &str) -> String {
    headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case(name))
        .map(|header| header.value.clone())
        .unwrap_or_default()
}

fn decode_state(bytes: &[u8]) -> Option<(u64, u64)> {
    if bytes.len() < STATE_LEN {
        return None;
    }
    let mut count = [0u8; 8];
    count.copy_from_slice(&bytes[..8]);
    let mut window_start = [0u8; 8];
    window_start.copy_from_slice(&bytes[8..STATE_LEN]);
    Some((u64::from_le_bytes(count), u64::from_le_bytes(window_start)))
}

fn encode_state(count: u64, window_start: u64) -> [u8; STATE_LEN] {
    let mut bytes = [0u8; STATE_LEN];
    bytes[..8].copy_from_slice(&count.to_le_bytes());
    bytes[8..].copy_from_slice(&window_start.to_le_bytes());
    bytes
}

fn record(outcome: &str) {
    if let Err(error) = host::incr_counter(
        "requests",
        &[("outcome".to_string(), outcome.to_string())],
        1,
    ) {
        host::log(
            types::LogLevel::Warn,
            &format!("local-ratelimit: cannot record metric: {error:?}"),
        );
    }
}

struct Component;

impl Lifecycle for Component {
    fn start(info: types::PluginInfo) -> Result<(), types::Error> {
        host::log(
            types::LogLevel::Info,
            &format!(
                "local-ratelimit {} starting on api {}",
                info.version, info.api_version
            ),
        );
        Ok(())
    }

    fn configure(config: String) -> Result<(), types::Error> {
        // `configure` runs once per distinct configuration, not per request.
        let parsed = parse(&config);
        if parsed.key == KeyPolicy::Header && parsed.header.is_empty() {
            return Err(types::Error::Invalid(
                "header is required when key=header".into(),
            ));
        }
        host::log(
            types::LogLevel::Info,
            &format!(
                "local-ratelimit: key={} header={} limit={} window={}s status={}",
                parsed.key.as_str(),
                if parsed.header.is_empty() {
                    "-"
                } else {
                    parsed.header.as_str()
                },
                parsed.limit,
                parsed.window_seconds,
                parsed.status
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
            let value = match config.key {
                KeyPolicy::Ip => req.client_ip.clone(),
                KeyPolicy::Header => header_value(&req.headers, &config.header),
                KeyPolicy::Route => req.route_id.clone(),
            };
            let shared_key = format!("{}:{}", config.key.as_str(), value);
            let window_ms = window_millis(config.window_seconds);
            let now = host::now_millis();
            // Shared data has no compare-and-swap, so concurrent workers can
            // lose an increment: the counter is an approximation, not a
            // transactional quota.
            let (mut count, mut window_start) = host::get_shared_data(NAMESPACE, &shared_key)
                .and_then(|bytes| decode_state(&bytes))
                .unwrap_or((0, now));
            // An expired or missing window starts over at this instant.
            if now.saturating_sub(window_start) >= window_ms {
                count = 0;
                window_start = now;
            }
            if count >= config.limit {
                let elapsed = now.saturating_sub(window_start);
                let remaining_ms = window_ms.saturating_sub(elapsed);
                let retry_after = remaining_ms.div_ceil(1000).max(1);
                record("limited");
                types::HeadersDecision::Reply(types::LocalReply {
                    status: config.status,
                    headers: vec![
                        types::Header {
                            name: "content-type".to_string(),
                            value: "application/json".to_string(),
                        },
                        types::Header {
                            name: "retry-after".to_string(),
                            value: retry_after.to_string(),
                        },
                    ],
                    body: ERROR_BODY.to_vec(),
                })
            } else {
                let next = count.saturating_add(1);
                if let Err(error) =
                    host::set_shared_data(NAMESPACE, &shared_key, &encode_state(next, window_start))
                {
                    // Losing the write must not deny traffic: log and allow.
                    host::log(
                        types::LogLevel::Warn,
                        &format!("local-ratelimit: cannot persist counter: {error:?}"),
                    );
                }
                record("allowed");
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
