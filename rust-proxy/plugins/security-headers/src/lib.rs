//! `io.example.security-headers` — response hardening headers, configured per
//! route.
//!
//! Why a plugin and not the built-in `request_headers` middleware: that one adds
//! fixed request headers. This one runs on the **response**, always overwrites
//! whatever the upstream sent (`set`, never `add`), and applies a small set of
//! zero-risk defaults so the common case needs no configuration at all.
//!
//! Configuration (JSON, opaque to the gateway):
//!
//! ```json
//! {
//!   "headers": {
//!     "strict-transport-security": "max-age=31536000; includeSubDomains",
//!     "content-security-policy": "default-src 'none'"
//!   },
//!   "disable_defaults": ["referrer-policy"]
//! }
//! ```
//!
//! `headers` wins over the defaults (same header name → the configured value is
//! what the client sees); `disable_defaults` removes a default instead of
//! overriding it. Nothing else is configurable on purpose: a header value is a
//! string, and a plugin that tries to be a policy engine is a plugin that will
//! change behaviour on upgrade.

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

/// Headers that are safe on every response and cannot break a working site.
///
/// `CSP` and `HSTS` are deliberately absent: a default CSP breaks almost any
/// real page, and HSTS is irreversible for the max-age an origin advertises
/// (and meaningless over plain HTTP). Both must be an explicit operator decision.
const DEFAULT_HEADERS: &[(&str, &str)] = &[
    ("x-content-type-options", "nosniff"),
    ("referrer-policy", "strict-origin-when-cross-origin"),
];

/// Header names the guest refuses to write, even when configured: they describe
/// framing of *this* response and are the host's to set.
const RESERVED_PREFIXES: &[&str] = &[
    "content-length",
    "transfer-encoding",
    "connection",
    "keep-alive",
    "upgrade",
    "te",
    "trailer",
];

#[derive(Default)]
struct Config {
    /// Final `(name, value)` list, defaults already merged in.
    headers: Vec<(String, String)>,
    /// Invalid configuration, kept so every response phase can report it once
    /// per exchange instead of trapping. `configure` returns the error anyway;
    /// this is the belt to that pair of braces.
    error: Option<String>,
}

thread_local! {
    static CONFIG: RefCell<Config> = const { RefCell::new(Config {
        headers: Vec::new(),
        error: None,
    }) };
}

/// A header name the host will accept: lowercase token characters only.
fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    name.len() <= 128
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// A header value the host will accept: visible ASCII plus space and tab, no
/// CR/LF (header injection), not empty.
fn valid_value(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 1024
        && value
            .bytes()
            .all(|b| b == b'\t' || (0x20..=0x7e).contains(&b))
}

fn reserved(name: &str) -> bool {
    RESERVED_PREFIXES.contains(&name)
}fn parse(raw: &str) -> Result<Config, String> {
    let material = if raw.trim().is_empty() { "{}" } else { raw };
    let doc: serde_json::Value =
        serde_json::from_str(material).map_err(|e| format!("config is not valid JSON: {e}"))?;
    let object = doc
        .as_object()
        .ok_or_else(|| "config must be a JSON object".to_string())?;

    let mut disallowed: Vec<String> = Vec::new();
    if let Some(list) = object.get("disable_defaults") {
        let list = list
            .as_array()
            .ok_or_else(|| "`disable_defaults` must be an array of strings".to_string())?;
        for entry in list {
            let name = entry
                .as_str()
                .ok_or_else(|| "`disable_defaults` must be an array of strings".to_string())?;
            if !DEFAULT_HEADERS.iter().any(|(default, _)| *default == name) {
                return Err(format!(
                    "`disable_defaults` names `{name}`, which is not a default header ({})",
                    DEFAULT_HEADERS
                        .iter()
                        .map(|(name, _)| *name)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            disallowed.push(name.to_string());
        }
    }

    let mut configured: Vec<(String, String)> = Vec::new();
    if let Some(map) = object.get("headers") {
        let map = map
            .as_object()
            .ok_or_else(|| "`headers` must be an object of name -> value".to_string())?;
        for (name, value) in map {
            let name = name.to_ascii_lowercase();
            let value = value
                .as_str()
                .ok_or_else(|| format!("`headers.{name}` must be a string"))?;
            if !valid_name(&name) {
                return Err(format!("`headers.{name}` is not a valid header name"));
            }
            if reserved(&name) {
                return Err(format!(
                    "`headers.{name}` is framing of this response and is not the plugin's to set"
                ));
            }
            // CORS has a built-in middleware and a warning for configuring it in
            // two places (`wasm::checks`). Refusing it here keeps one authority.
            if name.starts_with("access-control-") {
                return Err(format!(
                    "`headers.{name}` belongs to the built-in `cors` middleware; configure it there so CORS has one authority"
                ));
            }
            if !valid_value(value) {
                return Err(format!(
                    "`headers.{name}` is not a valid header value (no CR/LF, not empty)"
                ));
            }
            configured.push((name, value.to_string()));
        }
    }

    let mut headers = Vec::new();
    for (name, value) in DEFAULT_HEADERS {
        let overridden = configured.iter().any(|(configured, _)| configured == name);
        if overridden || disallowed.iter().any(|skip| skip == name) {
            continue;
        }
        headers.push(((*name).to_string(), (*value).to_string()));
    }
    headers.extend(configured);

    if headers.is_empty() {
        return Err("configuration leaves the plugin nothing to do".to_string());
    }
    Ok(Config {
        headers,
        error: None,
    })
}

struct Component;

impl Lifecycle for Component {
    fn start(_info: types::PluginInfo) -> Result<(), types::Error> {
        Ok(())
    }

    fn configure(config: String) -> Result<(), types::Error> {
        match parse(&config) {
            Ok(parsed) => {
                host::log(
                    types::LogLevel::Debug,
                    &format!("security-headers: {} header(s) configured", parsed.headers.len()),
                );
                CONFIG.with(|c| *c.borrow_mut() = parsed);
                Ok(())
            }
            Err(message) => {
                // A rejected configuration must be visible on every exchange,
                // not only in the log line the host writes once.
                let headers = match parse(&config) {
                    Ok(parsed) => parsed.headers,
                    Err(_) => Vec::new(),
                };
                CONFIG.with(|c| {
                    *c.borrow_mut() = Config {
                        headers,
                        error: Some(message.clone()),
                    }
                });
                Err(types::Error::Invalid(message))
            }
        }
    }

    fn stop() {}
}

impl RequestFilter for Component {
    fn on_request_headers(_req: types::Request) -> types::HeadersDecision {
        // This plugin only touches the response.
        types::HeadersDecision::Proceed(Vec::new())
    }

    fn on_request_body(chunk: types::BodyChunk) -> types::BodyDecision {
        types::BodyDecision::Proceed(chunk.bytes)
    }

    fn on_request_trailers(_trailers: Vec<types::Header>) -> types::HeadersDecision {
        types::HeadersDecision::Proceed(Vec::new())
    }
}

impl ResponseFilter for Component {
    fn on_response_headers(_resp: types::Response) -> types::HeadersDecision {
        let config = CONFIG.with(|c| {
            let config = c.borrow();
            (config.headers.clone(), config.error.clone())
        });
        let (headers, error) = config;
        if let Some(error) = error {
            host::log(
                types::LogLevel::Warn,
                &format!("security-headers: configuration was rejected: {error}"),
            );
            return types::HeadersDecision::Proceed(Vec::new());
        }
        let ops = headers
            .into_iter()
            .map(|(name, value)| {
                types::HeaderOp::Set(types::Header {
                    name,
                    value,
                })
            })
            .collect();
        types::HeadersDecision::Proceed(ops)
    }

    fn on_response_body(chunk: types::BodyChunk) -> types::BodyDecision {
        types::BodyDecision::Proceed(chunk.bytes)
    }

    fn on_response_trailers(_trailers: Vec<types::Header>) -> types::HeadersDecision {
        types::HeadersDecision::Proceed(Vec::new())
    }

    fn on_log(_summary: types::StreamSummary) {}
}

export!(Component);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_keeps_the_safe_defaults() {
        let parsed = parse("").unwrap();
        assert_eq!(parsed.headers.len(), DEFAULT_HEADERS.len());
        assert!(
            parsed
                .headers
                .iter()
                .any(|(name, value)| name == "x-content-type-options" && value == "nosniff")
        );
    }

    #[test]
    fn configured_headers_are_added_and_override_defaults() {
        let parsed = parse(r#"{"headers": {"referrer-policy": "no-referrer"}}"#).unwrap();
        let referrer: Vec<_> = parsed
            .headers
            .iter()
            .filter(|(name, _)| name == "referrer-policy")
            .collect();
        assert_eq!(referrer.len(), 1, "a default must not be duplicated");
        assert_eq!(referrer[0].1, "no-referrer");
    }

    #[test]
    fn disable_defaults_removes_only_known_defaults() {
        let parsed = parse(r#"{"disable_defaults": ["referrer-policy"]}"#).unwrap();
        assert!(
            parsed
                .headers
                .iter()
                .all(|(name, _)| name != "referrer-policy")
        );
        assert!(parse(r#"{"disable_defaults": ["x-made-up"]}"#).is_err());
    }

    #[test]
    fn rejects_hop_by_hop_and_malformed_headers() {
        assert!(parse(r#"{"headers": {"content-length": "10"}}"#).is_err());
        assert!(parse(r#"{"headers": {"X-Upper": "1"}}"#).is_ok());
        assert!(parse(r#"{"headers": {"x-a": "a\r\nb"}}"#).is_err());
        assert!(parse(r#"{"headers": {"x-a": ""}}"#).is_err());
        assert!(parse(r#"{"headers": {"bad name": "1"}}"#).is_err());
    }

    #[test]
    fn refuses_to_become_a_second_cors_authority() {
        let err = parse(r#"{"headers": {"access-control-allow-origin": "*"}}"#).unwrap_err();
        assert!(err.contains("cors"), "{err}");
    }

    #[test]
    fn rejects_config_that_does_nothing() {
        let everything: Vec<&str> = DEFAULT_HEADERS.iter().map(|(name, _)| *name).collect();
        let raw = serde_json::json!({ "disable_defaults": everything }).to_string();
        assert!(parse(&raw).is_err());
    }

    #[test]
    fn rejects_non_object_and_broken_json() {
        assert!(parse("[1,2]").is_err());
        assert!(parse("{not json").is_err());
    }
}
