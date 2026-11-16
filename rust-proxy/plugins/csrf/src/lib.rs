//! CSRF double-submit check for unsafe requests.
//!
//! Configuration is `key=value` lines, parsed once in `configure`:
//!
//! ```text
//! cookie=csrf
//! header=x-csrf-token
//! safe=GET,HEAD,OPTIONS
//! status=403
//! ```
//!
//! Safe methods pass through untouched. For every other method the plugin
//! takes the `<cookie name>=<value>` pair from the request's `cookie` header
//! and compares it, in constant time, with the value of the configured token
//! header. A missing or mismatching token is answered locally; the upstream
//! never sees the request.
//!
//! Build:
//!
//! ```sh
//! cargo build --release --target wasm32-wasip2
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

// Parsed configuration, written once by `configure` and only read per request.
// One instance is entered from one thread at a time.
thread_local! {
    static CONFIG: RefCell<Config> = RefCell::new(Config::default());
}

struct Config {
    /// Cookie name the token is expected in (`cookie`).
    cookie: String,
    /// Request header carrying the token (`header`).
    header: String,
    /// Methods treated as safe, upper case (`safe`).
    safe: Vec<String>,
    /// Status of the local rejection (`status`).
    status: u16,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            cookie: "csrf".to_string(),
            header: "x-csrf-token".to_string(),
            safe: vec!["GET".to_string(), "HEAD".to_string(), "OPTIONS".to_string()],
            status: 403,
        }
    }
}

const BODY: &str = "{\"error\":\"csrf token mismatch\"}";

/// Constant-time byte comparison: the token must not leak through timing.
/// Only the length is compared in variable time, which is unavoidable and
/// does not reveal the secret.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Extracts the first `<name>=<value>` pair from a `cookie` header value.
fn cookie_value(header_value: &str, name: &str) -> Option<String> {
    for part in header_value.split(';') {
        let part = part.trim();
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case(name) {
            return Some(value.trim().to_string());
        }
    }
    None
}

/// Parses the `key=value` configuration. Unknown keys are ignored so a newer
/// gateway can pass new keys to an older plugin.
fn parse(config: &str) -> Result<Config, types::Error> {
    let mut parsed = Config::default();
    for line in config.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "cookie" => {
                if value.is_empty() {
                    return Err(types::Error::Invalid("cookie must not be empty".into()));
                }
                parsed.cookie = value.to_ascii_lowercase();
            }
            "header" => {
                if value.is_empty() {
                    return Err(types::Error::Invalid("header must not be empty".into()));
                }
                parsed.header = value.to_ascii_lowercase();
            }
            "safe" => {
                let methods: Vec<String> = value
                    .split(',')
                    .map(|method| method.trim().to_ascii_uppercase())
                    .filter(|method| !method.is_empty())
                    .collect();
                if methods.is_empty() {
                    return Err(types::Error::Invalid(
                        "safe must list at least one method".into(),
                    ));
                }
                parsed.safe = methods;
            }
            "status" => {
                parsed.status = value.parse::<u16>().map_err(|_| {
                    types::Error::Invalid(format!("status `{value}` is not a valid u16"))
                })?;
            }
            _ => {}
        }
    }
    Ok(parsed)
}

/// Local rejection shared by the mismatch paths.
fn reject(config: &Config, reason: &str) -> types::HeadersDecision {
    host::log(
        types::LogLevel::Debug,
        &format!("csrf token rejected ({reason})"),
    );
    types::HeadersDecision::Reply(types::LocalReply {
        status: config.status,
        headers: vec![types::Header {
            name: "content-type".to_string(),
            value: "application/json".to_string(),
        }],
        body: BODY.as_bytes().to_vec(),
    })
}

struct Component;

impl Lifecycle for Component {
    fn start(info: types::PluginInfo) -> Result<(), types::Error> {
        host::log(
            types::LogLevel::Info,
            &format!(
                "csrf {} starting on api {} with {:?}",
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
                "csrf protecting unsafe methods with cookie `{}` and header `{}`",
                parsed.cookie, parsed.header
            ),
        );
        CONFIG.with(|slot| *slot.borrow_mut() = parsed);
        Ok(())
    }

    fn stop() {}
}

impl RequestFilter for Component {
    fn on_request_headers(req: types::Request) -> types::HeadersDecision {
        CONFIG.with(|slot| {
            let config = slot.borrow();
            if config
                .safe
                .iter()
                .any(|method| method.eq_ignore_ascii_case(&req.method))
            {
                return types::HeadersDecision::Proceed(Vec::new());
            }

            let Some(cookie_header) = req
                .headers
                .iter()
                .find(|header| header.name.eq_ignore_ascii_case("cookie"))
            else {
                return reject(&config, "no cookie header");
            };
            let Some(cookie_token) = cookie_value(&cookie_header.value, &config.cookie) else {
                return reject(&config, "no token cookie");
            };

            let Some(header_token) = req
                .headers
                .iter()
                .find(|header| header.name.eq_ignore_ascii_case(&config.header))
                .map(|header| header.value.as_str())
            else {
                return reject(&config, "no token header");
            };

            if constant_time_eq(cookie_token.as_bytes(), header_token.as_bytes()) {
                types::HeadersDecision::Proceed(Vec::new())
            } else {
                reject(&config, "token mismatch")
            }
        })
    }

    fn on_request_body(chunk: types::BodyChunk) -> types::BodyDecision {
        // Unchanged pass-through; only reached on body-filtering routes.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_defaults_and_overrides() {
        let config = parse("cookie=sess\nheader=x-token\nstatus=419\n").unwrap();
        assert_eq!(config.cookie, "sess");
        assert_eq!(config.header, "x-token");
        assert_eq!(config.status, 419);
        assert_eq!(config.safe, vec!["GET", "HEAD", "OPTIONS"]);
    }

    #[test]
    fn rejects_bad_status_and_empty_safe() {
        assert!(parse("status=nope").is_err());
        assert!(parse("safe= , ").is_err());
    }

    #[test]
    fn extracts_first_matching_cookie() {
        assert_eq!(
            cookie_value("a=1; csrf=abc; csrf=later", "csrf").as_deref(),
            Some("abc")
        );
        assert_eq!(cookie_value("a=1; b=2", "csrf"), None);
    }

    #[test]
    fn constant_time_equality_matches_plain_equality() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
