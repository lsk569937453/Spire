//! Client-IP tagging.
//!
//! Matches the resolved client IP (`req.client_ip`, after `trusted_proxies`)
//! against ordered IPv4 CIDR rules and **adds** the configured tag header, so a
//! value already sent by the peer is preserved.
//!
//! Configuration is `key=value` lines (opaque to the gateway). A `tag=` line
//! applies to the `cidr=` lines that follow it, and groups repeat:
//!
//! ```text
//! header=x-ip-tag
//! tag=internal
//! cidr=10.0.0.0/8
//! cidr=192.168.0.0/16
//! tag=partner
//! cidr=203.0.113.0/24
//! default=external
//! ```
//!
//! Only IPv4 ranges are supported; IPv6 ranges are ignored with a warning.
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

const DEFAULT_HEADER: &str = "x-ip-tag";

thread_local! {
    static CONFIG: RefCell<Config> = RefCell::new(Config::defaults());
}

#[derive(Clone)]
struct Rule {
    tag: String,
    /// Network address already masked with `mask`.
    network: u32,
    mask: u32,
}

#[derive(Clone)]
struct Config {
    header: String,
    rules: Vec<Rule>,
    default_tag: Option<String>,
}

impl Config {
    fn defaults() -> Config {
        Config {
            header: DEFAULT_HEADER.to_string(),
            rules: Vec::new(),
            default_tag: None,
        }
    }
}

/// Parses `key=value` lines. Unknown keys are ignored so a newer gateway can
/// pass extra keys to an older plugin.
fn parse(config: &str) -> Config {
    let mut parsed = Config::defaults();
    let mut current_tag: Option<String> = None;
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
            "tag" => {
                let tag = value.trim();
                if !tag.is_empty() {
                    current_tag = Some(tag.to_string());
                }
            }
            "cidr" => {
                let spec = value.trim();
                if spec.is_empty() {
                    continue;
                }
                if spec.contains(':') {
                    host::log(
                        types::LogLevel::Warn,
                        &format!("ip-tagging: IPv6 CIDR '{spec}' is not supported, ignored"),
                    );
                    continue;
                }
                let Some(tag) = current_tag.as_ref() else {
                    host::log(
                        types::LogLevel::Warn,
                        &format!("ip-tagging: CIDR '{spec}' has no preceding tag= line, ignored"),
                    );
                    continue;
                };
                match parse_cidr(spec) {
                    Some((network, mask)) => parsed.rules.push(Rule {
                        tag: tag.clone(),
                        network,
                        mask,
                    }),
                    None => host::log(
                        types::LogLevel::Warn,
                        &format!("ip-tagging: invalid IPv4 CIDR '{spec}', ignored"),
                    ),
                }
            }
            "default" => {
                let tag = value.trim();
                if !tag.is_empty() {
                    parsed.default_tag = Some(tag.to_string());
                }
            }
            _ => {}
        }
    }
    parsed
}

/// Parses `a.b.c.d/prefix` into a masked network address and its mask.
fn parse_cidr(spec: &str) -> Option<(u32, u32)> {
    let (address, prefix) = spec.split_once('/')?;
    let base = parse_ipv4(address.trim())?;
    let prefix: u32 = prefix.trim().parse().ok()?;
    if prefix > 32 {
        return None;
    }
    // `checked_shl` returns `None` for a shift of 32, which is exactly the
    // prefix length 0 case where every address matches.
    let mask = u32::MAX.checked_shl(32 - prefix).unwrap_or(0);
    Some((base & mask, mask))
}

fn parse_ipv4(address: &str) -> Option<u32> {
    let mut value: u32 = 0;
    let mut octets = 0;
    for part in address.split('.') {
        if octets == 4 || part.is_empty() || part.len() > 3 {
            return None;
        }
        if !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let octet: u32 = part.parse().ok()?;
        if octet > 255 {
            return None;
        }
        value = (value << 8) | octet;
        octets += 1;
    }
    if octets == 4 {
        Some(value)
    } else {
        None
    }
}

/// Parses the resolved client IP, also accepting an IPv4-mapped IPv6 address
/// such as `::ffff:10.0.0.1`.
fn client_ipv4(raw: &str) -> Option<u32> {
    let lowered = raw.trim().to_ascii_lowercase();
    let candidate = lowered.strip_prefix("::ffff:").unwrap_or(&lowered);
    parse_ipv4(candidate)
}

struct Component;

impl Lifecycle for Component {
    fn start(info: types::PluginInfo) -> Result<(), types::Error> {
        host::log(
            types::LogLevel::Info,
            &format!(
                "ip-tagging {} starting on api {}",
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
                "ip-tagging: header '{}' with {} CIDR rule(s), default={}",
                parsed.header,
                parsed.rules.len(),
                parsed.default_tag.as_deref().unwrap_or("-")
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
            let tag = client_ipv4(&req.client_ip)
                .and_then(|address| {
                    config
                        .rules
                        .iter()
                        .find(|rule| (address & rule.mask) == rule.network)
                        .map(|rule| rule.tag.clone())
                })
                .or_else(|| config.default_tag.clone());
            match tag {
                Some(tag) => {
                    types::HeadersDecision::Proceed(vec![types::HeaderOp::Add(types::Header {
                        name: config.header.clone(),
                        value: tag,
                    })])
                }
                None => types::HeadersDecision::Proceed(Vec::new()),
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
