//! HTTP Basic authentication.
//!
//! Decodes `Authorization: Basic <base64>`, splits the credential at the first
//! `:`, and compares user and password in constant time against a configured
//! list. A missing or wrong credential is answered with a local `401` carrying
//! a `www-authenticate` challenge.
//!
//! Configuration is `key=value` lines (opaque to the gateway), e.g.
//!
//! ```text
//! realm=spire
//! users=alice:secret,bob:pw
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

use base64::Engine as _;

wit_bindgen::generate!({
    world: "filter-plugin",
    path: "wit",
});

use exports::spire::filter::lifecycle::Guest as Lifecycle;
use exports::spire::filter::request_filter::Guest as RequestFilter;
use exports::spire::filter::response_filter::Guest as ResponseFilter;
use spire::filter::host;
use spire::filter::types;

const DEFAULT_REALM: &str = "spire";
const UNAUTHORIZED_BODY: &[u8] = b"unauthorized";

thread_local! {
    static CONFIG: RefCell<Config> = RefCell::new(Config::defaults());
}

#[derive(Clone)]
struct Config {
    realm: String,
    users: Vec<(String, String)>,
    strip: bool,
}

impl Config {
    fn defaults() -> Config {
        Config {
            realm: DEFAULT_REALM.to_string(),
            users: Vec::new(),
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
            "realm" => {
                let realm = value.trim();
                if !realm.is_empty() {
                    parsed.realm = realm.to_string();
                }
            }
            "users" => parsed.users = parse_users(value),
            "strip" => parsed.strip = parse_bool(value.trim()),
            _ => {}
        }
    }
    parsed
}

/// `alice:secret,bob:pw` -> `[("alice", "secret"), ("bob", "pw")]`.
fn parse_users(value: &str) -> Vec<(String, String)> {
    value
        .split(',')
        .filter_map(|entry| {
            let entry = entry.trim();
            let (user, password) = entry.split_once(':')?;
            let user = user.trim();
            if user.is_empty() {
                return None;
            }
            Some((user.to_string(), password.trim().to_string()))
        })
        .collect()
}

fn parse_bool(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "true" | "1" | "yes" | "on"
    )
}

/// Decodes the credentials of an `Authorization: Basic <base64>` value.
fn decode_credentials(value: &str) -> Option<String> {
    let (scheme, encoded) = value.trim().split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .ok()?;
    String::from_utf8(decoded).ok()
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

fn header_value(headers: &[types::Header], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case(name))
        .map(|header| header.value.clone())
}

/// Checks a decoded `user:password` credential against every configured pair
/// without stopping at the first mismatch.
fn verify(config: &Config, credentials: &str) -> bool {
    let Some((user, password)) = credentials.split_once(':') else {
        return false;
    };
    let mut matched = false;
    for (known_user, known_password) in &config.users {
        let user_eq = constant_time_eq(known_user.as_bytes(), user.as_bytes());
        let password_eq = constant_time_eq(known_password.as_bytes(), password.as_bytes());
        matched |= user_eq && password_eq;
    }
    matched
}

/// Strips characters that would break out of the quoted realm parameter.
fn sanitize_realm(realm: &str) -> String {
    realm
        .chars()
        .filter(|character| !matches!(character, '"' | '\\' | '\r' | '\n'))
        .collect()
}

fn reject(realm: &str) -> types::HeadersDecision {
    types::HeadersDecision::Reply(types::LocalReply {
        status: 401,
        headers: vec![
            types::Header {
                name: "content-type".to_string(),
                value: "text/plain; charset=utf-8".to_string(),
            },
            types::Header {
                name: "www-authenticate".to_string(),
                value: format!("Basic realm=\"{}\"", sanitize_realm(realm)),
            },
        ],
        body: UNAUTHORIZED_BODY.to_vec(),
    })
}

struct Component;

impl Lifecycle for Component {
    fn start(info: types::PluginInfo) -> Result<(), types::Error> {
        host::log(
            types::LogLevel::Info,
            &format!(
                "basic-auth {} starting on api {}",
                info.version, info.api_version
            ),
        );
        Ok(())
    }

    fn configure(config: String) -> Result<(), types::Error> {
        // `configure` runs once per distinct configuration, not per request.
        let parsed = parse(&config);
        if parsed.users.is_empty() {
            return Err(types::Error::Invalid(
                "users is required and must list at least one user:password pair".into(),
            ));
        }
        host::log(
            types::LogLevel::Info,
            &format!(
                "basic-auth: realm '{}' with {} user(s), strip={}",
                parsed.realm,
                parsed.users.len(),
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
            let authorized = header_value(&req.headers, "authorization")
                .and_then(|value| decode_credentials(&value))
                .map(|credentials| verify(&config, &credentials))
                .unwrap_or(false);
            if !authorized {
                host::log(
                    types::LogLevel::Debug,
                    "basic-auth: missing or invalid credentials",
                );
                return reject(&config.realm);
            }
            if config.strip {
                types::HeadersDecision::Proceed(vec![types::HeaderOp::Remove(
                    "authorization".to_string(),
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
