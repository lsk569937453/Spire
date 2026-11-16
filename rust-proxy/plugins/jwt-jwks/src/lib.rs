//! JWT validation against a published JWKS document.
//!
//! On every request the plugin takes a `Bearer` token out of a configured
//! header, reads `kid`/`alg` from its JOSE header, and verifies the signature
//! with the matching RSA public key from the tenant's JWKS document.
//!
//! The JWKS document is fetched through `host.http-call` at most once per
//! `cache_seconds` and cached in shared data, so every worker thread and every
//! route sharing the same `jwks_url` reuses one copy:
//!
//! ```text
//! namespace = "jwks"
//! key       = jwks_url
//! value     = 8-byte little-endian expiry (millis since the epoch) || JSON
//! ```
//!
//! Configuration is `key=value` lines, parsed exactly once in `configure`:
//!
//! ```text
//! jwks_url=https://idp.internal/.well-known/jwks.json   # required
//! issuer=https://issuer.example                         # optional
//! audience=spire-api                                    # optional
//! alg=RS256                                             # default RS256
//! cache_seconds=300                                     # default 300, capped at 3600
//! header=authorization                                  # default authorization
//! strip=false                                           # default false
//! ```
//!
//! Every failure is a local `401` with `www-authenticate: Bearer`; the plugin
//! never forwards a token it could not verify, and it never caches a JWKS
//! response that was not 2xx.
//!
//! Build:
//!
//! ```sh
//! rustup target add wasm32-wasip2
//! cargo build --release --target wasm32-wasip2
//! # dist/plugin.wasm is the component the gateway loads
//! ```

use std::cell::RefCell;
use std::str::FromStr;

wit_bindgen::generate!({
    world: "filter-plugin",
    path: "wit",
});

use exports::spire::filter::lifecycle::Guest as Lifecycle;
use exports::spire::filter::request_filter::Guest as RequestFilter;
use exports::spire::filter::response_filter::Guest as ResponseFilter;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use spire::filter::host;
use spire::filter::types;
use types::Header;

// Per-instance state. Kept in a `thread_local` because the component model
// gives each instance its own linear memory but not a place to hang state;
// one instance is only ever entered from one thread at a time.
thread_local! {
    static CONFIG: RefCell<Option<Config>> = const { RefCell::new(None) };
}

/// Shared-data namespace for cached JWKS documents.
const JWKS_NAMESPACE: &str = "jwks";
/// Size of the little-endian expiry prefix in a cached JWKS value.
const CACHE_HEADER_LEN: usize = 8;
/// Default JWKS cache lifetime, seconds.
const DEFAULT_CACHE_SECONDS: u64 = 300;
/// Hard cap on the cache lifetime, so a rotated key is picked up eventually.
const MAX_CACHE_SECONDS: u64 = 3600;
/// Body of every local `401` reply.
const INVALID_TOKEN_BODY: &[u8] = br#"{"error":"invalid token"}"#;

/// Parsed once per distinct `config:` blob. `Validation` is built here too:
/// rebuilding it per request would allocate several hash sets on the hot path.
struct Config {
    jwks_url: String,
    alg: Algorithm,
    validation: Validation,
    cache_seconds: u64,
    /// Lowercased header name; request header names reach the plugin lowercased.
    header: String,
    /// Remove the credential from the request before it reaches the upstream.
    strip: bool,
}

/// Why a request was rejected, which is also its metric outcome.
#[derive(Clone, Copy)]
enum Rejection {
    /// The token itself is at fault: absent, malformed, unsigned by the tenant.
    Invalid,
    /// The plugin could not do its job: JWKS unreachable or unusable.
    Failure,
}

/// Parses a `true`/`false`-style boolean. Unknown spellings are rejected so a
/// typo cannot silently flip `strip`.
fn parse_bool(name: &str, value: &str) -> Result<bool, types::Error> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        other => Err(types::Error::Invalid(format!(
            "{name} must be true or false, not `{other}`"
        ))),
    }
}

/// Only RSA-family algorithms can be verified from `n`/`e` JWK components,
/// which is what a JWKS document offers for RS*/PS* tokens.
fn is_rsa_algorithm(alg: Algorithm) -> bool {
    matches!(
        alg,
        Algorithm::RS256
            | Algorithm::RS384
            | Algorithm::RS512
            | Algorithm::PS256
            | Algorithm::PS384
            | Algorithm::PS512
    )
}

/// Parses the `key=value` blob. Unknown keys are rejected rather than ignored:
/// a typo in `strip` or `alg` must not silently weaken validation.
fn parse(config: &str) -> Result<Config, types::Error> {
    let mut jwks_url: Option<String> = None;
    let mut issuer: Option<String> = None;
    let mut audience: Option<String> = None;
    let mut alg_name = "RS256".to_string();
    let mut cache_seconds = DEFAULT_CACHE_SECONDS;
    let mut header = "authorization".to_string();
    let mut strip = false;

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
            "jwks_url" => {
                if value.is_empty() {
                    return Err(types::Error::Invalid(
                        "jwks_url must not be empty".to_string(),
                    ));
                }
                jwks_url = Some(value.to_string());
            }
            "issuer" => {
                if !value.is_empty() {
                    issuer = Some(value.to_string());
                }
            }
            "audience" => {
                if !value.is_empty() {
                    audience = Some(value.to_string());
                }
            }
            "alg" => {
                if value.is_empty() {
                    return Err(types::Error::Invalid("alg must not be empty".to_string()));
                }
                alg_name = value.to_string();
            }
            "cache_seconds" => {
                cache_seconds = value.parse::<u64>().map_err(|_| {
                    types::Error::Invalid(format!(
                        "cache_seconds must be a whole number of seconds, not `{value}`"
                    ))
                })?;
            }
            "header" => {
                if value.is_empty() {
                    return Err(types::Error::Invalid(
                        "header must not be empty".to_string(),
                    ));
                }
                header = value.to_ascii_lowercase();
            }
            "strip" => strip = parse_bool("strip", value)?,
            other => {
                return Err(types::Error::Invalid(format!("unknown key `{other}`")));
            }
        }
    }

    let jwks_url =
        jwks_url.ok_or_else(|| types::Error::Invalid("jwks_url is required".to_string()))?;
    if !(jwks_url.starts_with("http://") || jwks_url.starts_with("https://")) {
        return Err(types::Error::Invalid(format!(
            "jwks_url must be an absolute http(s) URL, got `{jwks_url}`"
        )));
    }
    let alg = Algorithm::from_str(&alg_name)
        .map_err(|_| types::Error::Invalid(format!("unknown alg `{alg_name}`")))?;
    if !is_rsa_algorithm(alg) {
        return Err(types::Error::Invalid(format!(
            "alg `{alg_name}` is not an RSA algorithm; a JWKS document only carries RSA keys"
        )));
    }

    let mut validation = Validation::new(alg);
    let mut required = vec!["exp"];
    if let Some(issuer) = issuer.as_deref() {
        validation.set_issuer(&[issuer]);
        required.push("iss");
    }
    if let Some(audience) = audience.as_deref() {
        validation.set_audience(&[audience]);
        required.push("aud");
    }
    // A configured iss/aud that the token may omit would be a bypass, so both
    // become required claims once the operator names them.
    validation.set_required_spec_claims(&required);

    Ok(Config {
        jwks_url,
        alg,
        validation,
        cache_seconds,
        header,
        strip,
    })
}

/// The bearer token from the configured header, if it is there and well formed.
fn bearer_token(headers: &[types::Header], name: &str) -> Option<String> {
    let value = headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case(name))?
        .value
        .trim();
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}

/// A fresh JWKS document from shared data, or `None` when absent or expired.
fn cached_jwks(url: &str, now: u64) -> Option<Vec<u8>> {
    let raw = host::get_shared_data(JWKS_NAMESPACE, url)?;
    let head = raw.get(..CACHE_HEADER_LEN)?;
    let expires = u64::from_le_bytes(head.try_into().ok()?);
    let json = raw.get(CACHE_HEADER_LEN..)?;
    if now < expires && !json.is_empty() {
        Some(json.to_vec())
    } else {
        None
    }
}

/// Stores `expiry || json` in shared data. A failed store only costs a refetch
/// later, so it degrades to a warning instead of failing the request.
fn cache_jwks(url: &str, expires: u64, json: &[u8]) {
    let mut value = Vec::with_capacity(CACHE_HEADER_LEN + json.len());
    value.extend_from_slice(&expires.to_le_bytes());
    value.extend_from_slice(json);
    if let Err(e) = host::set_shared_data(JWKS_NAMESPACE, url, &value) {
        host::log(
            types::LogLevel::Warn,
            &format!("cannot cache JWKS for {url}: {e:?}"),
        );
    }
}

/// Fetches the JWKS document. Redirects are not followed by the host, so a
/// 3xx is a failure rather than something to chase.
fn fetch_jwks(url: &str) -> Result<Vec<u8>, String> {
    let request = types::HttpRequest {
        method: "GET".to_string(),
        url: url.to_string(),
        headers: vec![Header {
            name: "accept".to_string(),
            value: "application/json".to_string(),
        }],
        body: None,
    };
    match host::http_call(&request) {
        Ok(response) if (200..300).contains(&response.status) => Ok(response.body),
        Ok(response) => Err(format!("JWKS endpoint answered {}", response.status)),
        Err(e) => Err(format!("JWKS fetch failed: {e:?}")),
    }
}

/// Returns a usable JWKS document, from the cache when it is still fresh.
fn load_jwks(config: &Config, now: u64) -> Result<Vec<u8>, String> {
    if let Some(json) = cached_jwks(&config.jwks_url, now) {
        return Ok(json);
    }
    let json = fetch_jwks(&config.jwks_url)?;
    let ttl_millis = config
        .cache_seconds
        .min(MAX_CACHE_SECONDS)
        .saturating_mul(1000);
    cache_jwks(&config.jwks_url, now.saturating_add(ttl_millis), &json);
    Ok(json)
}

/// `(n, e)` of the RSA signing key identified by `kid`.
fn rsa_components<'a>(jwks: &'a serde_json::Value, kid: &str) -> Option<(&'a str, &'a str)> {
    let keys = jwks.get("keys")?.as_array()?;
    for key in keys {
        if key.get("kid").and_then(serde_json::Value::as_str) != Some(kid) {
            continue;
        }
        if key.get("kty").and_then(serde_json::Value::as_str) != Some("RSA") {
            continue;
        }
        match key.get("use").and_then(serde_json::Value::as_str) {
            Some("sig") | None => {}
            Some(_) => continue,
        }
        let (Some(n), Some(e)) = (
            key.get("n").and_then(serde_json::Value::as_str),
            key.get("e").and_then(serde_json::Value::as_str),
        ) else {
            continue;
        };
        return Some((n, e));
    }
    None
}

fn record(outcome: &str) {
    if let Err(e) = host::incr_counter(
        "token_checks",
        &[("outcome".to_string(), outcome.to_string())],
        1,
    ) {
        host::log(
            types::LogLevel::Warn,
            &format!("cannot record metric: {e:?}"),
        );
    }
}

/// The single local reply for every rejection: `401` plus a bearer challenge.
fn reject(kind: Rejection, detail: Option<&str>) -> types::HeadersDecision {
    match kind {
        Rejection::Invalid => {
            if let Some(detail) = detail {
                host::log(types::LogLevel::Debug, &format!("token rejected: {detail}"));
            }
            record("invalid");
        }
        Rejection::Failure => {
            // Never silent: an unreachable JWKS is an operator-visible failure.
            host::log(
                types::LogLevel::Warn,
                &format!(
                    "token check failed: {}",
                    detail.unwrap_or("JWKS unavailable")
                ),
            );
            record("error");
        }
    }
    types::HeadersDecision::Reply(types::LocalReply {
        status: 401,
        headers: vec![
            Header {
                name: "content-type".to_string(),
                value: "application/json".to_string(),
            },
            Header {
                name: "www-authenticate".to_string(),
                value: "Bearer".to_string(),
            },
        ],
        body: INVALID_TOKEN_BODY.to_vec(),
    })
}

/// Verifies the token and forwards the request, stripping the credential when
/// configured to.
fn evaluate(config: &Config, req: &types::Request) -> types::HeadersDecision {
    let Some(token) = bearer_token(&req.headers, &config.header) else {
        return reject(Rejection::Invalid, Some("no Bearer token"));
    };

    let header = match decode_header(&token) {
        Ok(header) => header,
        Err(e) => {
            return reject(
                Rejection::Invalid,
                Some(&format!("unreadable JOSE header: {e}")),
            )
        }
    };
    // A token may only be checked with the algorithm the operator pinned; this
    // is what rules out `alg: none` and algorithm-confusion attacks.
    if header.alg != config.alg {
        return reject(
            Rejection::Invalid,
            Some(&format!(
                "alg {:?} does not match the configured algorithm",
                header.alg
            )),
        );
    }
    let Some(kid) = header.kid else {
        return reject(Rejection::Invalid, Some("JOSE header has no kid"));
    };

    let now = host::now_millis();
    let jwks_bytes = match load_jwks(config, now) {
        Ok(jwks) => jwks,
        Err(e) => return reject(Rejection::Failure, Some(&e)),
    };
    let jwks: serde_json::Value = match serde_json::from_slice(&jwks_bytes) {
        Ok(jwks) => jwks,
        Err(e) => return reject(Rejection::Failure, Some(&format!("JWKS is not JSON: {e}"))),
    };
    let Some((n, e)) = rsa_components(&jwks, &kid) else {
        return reject(
            Rejection::Invalid,
            Some(&format!("no RSA signing key for kid `{kid}`")),
        );
    };
    let key = match DecodingKey::from_rsa_components(n, e) {
        Ok(key) => key,
        Err(err) => {
            return reject(
                Rejection::Failure,
                Some(&format!("invalid RSA key for kid `{kid}`: {err}")),
            )
        }
    };

    match decode::<serde_json::Value>(&token, &key, &config.validation) {
        Ok(_) => {
            record("ok");
            if config.strip {
                // Remove the configured credential header, not a hardcoded
                // `authorization`: leaving the token in a custom header would
                // defeat the point of stripping it.
                types::HeadersDecision::Proceed(vec![types::HeaderOp::Remove(
                    config.header.clone(),
                )])
            } else {
                types::HeadersDecision::Proceed(Vec::new())
            }
        }
        Err(err) => reject(
            Rejection::Invalid,
            Some(&format!("signature or claims rejected: {err}")),
        ),
    }
}

struct Component;

impl Lifecycle for Component {
    fn start(info: types::PluginInfo) -> Result<(), types::Error> {
        host::log(
            types::LogLevel::Info,
            &format!(
                "jwt-jwks {} starting on api {} with {:?}",
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
                "verifying {:?} tokens against {}, cache {}s, strip {}",
                parsed.alg, parsed.jwks_url, parsed.cache_seconds, parsed.strip
            ),
        );
        CONFIG.with(|cell| *cell.borrow_mut() = Some(parsed));
        Ok(())
    }

    fn stop() {
        // The JWKS cache lives in shared data, not in this instance.
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
                    // `configure` always runs first; without it nothing can be
                    // verified, so refuse rather than forward.
                    reject(Rejection::Failure, Some("plugin is not configured"))
                }
            }
        })
    }

    fn on_request_body(chunk: types::BodyChunk) -> types::BodyDecision {
        // Only the headers are needed to verify a token.
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
