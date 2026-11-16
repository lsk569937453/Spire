//! Asymmetric JWT check for `Bearer` tokens.
//!
//! Configuration is `key=value` lines, parsed once in `configure`:
//!
//! ```text
//! alg=RS256
//! key_base64=<base64 of the public key DER>
//! issuer=https://issuer.example
//! audience=spire-gateway
//! header=authorization
//! strip=false
//! ```
//!
//! `key_base64` is the DER form of the **SubjectPublicKeyInfo** (the body of a
//! `PUBLIC KEY` PEM block). The plugin extracts the inner key material for the
//! verifier: the PKCS#1 `RSAPublicKey` for RS256, the SEC1 point for ES256. A
//! key that is already in that inner form is accepted as well.
//!
//! Every request must carry a valid token; a missing, malformed or expired
//! token is answered with `401` and `www-authenticate: Bearer`. On success the
//! request is forwarded, optionally with the credential header removed.
//!
//! Build:
//!
//! ```sh
//! cargo build --release --target wasm32-wasip2
//! ```

use std::cell::RefCell;

use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE_NO_PAD};
use base64::Engine;

use jsonwebtoken::{Algorithm, DecodingKey, Validation};

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

#[derive(Clone, Copy)]
enum KeyAlg {
    Rs256,
    Es256,
}

impl KeyAlg {
    fn algorithm(self) -> Algorithm {
        match self {
            KeyAlg::Rs256 => Algorithm::RS256,
            KeyAlg::Es256 => Algorithm::ES256,
        }
    }
}

struct Verifier {
    key: DecodingKey,
    validation: Validation,
}

struct Config {
    /// Request header carrying the token (`header`).
    header: String,
    /// Remove the credential header before forwarding (`strip`).
    strip: bool,
    /// Built in `configure`; `none` only before the first successful one.
    verifier: Option<Verifier>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            header: "authorization".to_string(),
            strip: false,
            verifier: None,
        }
    }
}

const BODY: &str = "{\"error\":\"invalid token\"}";

/// Reads one DER TLV with the given tag and returns `(value, next offset)`.
fn read_tlv(input: &[u8], tag: u8) -> Option<(&[u8], usize)> {
    if *input.first()? != tag {
        return None;
    }
    let (length, length_bytes) = read_length(input.get(1..)?)?;
    let start = 1 + length_bytes;
    let end = start.checked_add(length)?;
    Some((input.get(start..end)?, end))
}

/// Reads a DER length, supporting the short form and the two-octet long form
/// that key material of this size needs.
fn read_length(input: &[u8]) -> Option<(usize, usize)> {
    let first = *input.first()? as usize;
    if first < 0x80 {
        return Some((first, 1));
    }
    let count = first & 0x7f;
    if count == 0 || count > 2 {
        return None;
    }
    let mut length = 0usize;
    for index in 0..count {
        length = (length << 8) | *input.get(1 + index)? as usize;
    }
    Some((length, 1 + count))
}

/// Extracts the key bits of a `SubjectPublicKeyInfo`: `none` when the input is
/// not one, in which case the caller keeps it as provided.
fn spki_subject_public_key(der: &[u8]) -> Option<&[u8]> {
    // SubjectPublicKeyInfo ::= SEQUENCE { algorithm AlgorithmIdentifier,
    //                                     subjectPublicKey BIT STRING }
    let (body, _) = read_tlv(der, 0x30)?;
    let (_, after_algorithm) = read_tlv(body, 0x30)?;
    let (bits, _) = read_tlv(body.get(after_algorithm..)?, 0x03)?;
    // A public key never uses unused bits, so the mandatory first octet is 0.
    if *bits.first()? != 0 {
        return None;
    }
    bits.get(1..)
}

/// Decodes base64, tolerating the padding and alphabet variants a copy-pasted
/// PEM body tends to carry.
fn decode_key_base64(value: &str) -> Result<Vec<u8>, types::Error> {
    let compact: String = value.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.is_empty() {
        return Err(types::Error::Invalid("key_base64 must not be empty".into()));
    }
    for engine in [&STANDARD, &STANDARD_NO_PAD, &URL_SAFE_NO_PAD] {
        if let Ok(bytes) = engine.decode(&compact) {
            return Ok(bytes);
        }
    }
    Err(types::Error::Invalid(
        "key_base64 is not valid base64".into(),
    ))
}

/// Splits `Bearer <token>` without allocating.
fn bearer_token(value: &str) -> Option<&str> {
    let mut parts = value.splitn(2, char::is_whitespace);
    let scheme = parts.next()?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = parts.next()?.trim();
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

fn parse_bool(value: &str) -> Result<bool, types::Error> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "1" | "on" => Ok(true),
        "false" | "no" | "0" | "off" => Ok(false),
        other => Err(types::Error::Invalid(format!(
            "strip `{other}` is not a boolean"
        ))),
    }
}

/// Parses the `key=value` configuration. Unknown keys are ignored so a newer
/// gateway can pass new keys to an older plugin.
fn parse(config: &str) -> Result<Config, types::Error> {
    let mut parsed = Config::default();
    let mut alg = KeyAlg::Rs256;
    let mut key_base64: Option<&str> = None;
    let mut issuer: Option<&str> = None;
    let mut audience: Option<&str> = None;

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
            "alg" => {
                alg = match value.to_ascii_uppercase().as_str() {
                    "RS256" => KeyAlg::Rs256,
                    "ES256" => KeyAlg::Es256,
                    other => {
                        return Err(types::Error::Invalid(format!(
                            "alg `{other}` must be RS256 or ES256"
                        )))
                    }
                };
            }
            "key_base64" => key_base64 = Some(value),
            "issuer" => issuer = Some(value).filter(|value| !value.is_empty()),
            "audience" => audience = Some(value).filter(|value| !value.is_empty()),
            "header" => {
                if value.is_empty() {
                    return Err(types::Error::Invalid("header must not be empty".into()));
                }
                parsed.header = value.to_ascii_lowercase();
            }
            "strip" => parsed.strip = parse_bool(value)?,
            _ => {}
        }
    }

    let Some(key_base64) = key_base64 else {
        return Err(types::Error::Invalid("key_base64 is required".into()));
    };
    let der = decode_key_base64(key_base64)?;
    let key_bytes = match spki_subject_public_key(&der) {
        Some(inner) => inner.to_vec(),
        None => der,
    };

    let key = match alg {
        KeyAlg::Rs256 => DecodingKey::from_rsa_der(&key_bytes),
        KeyAlg::Es256 => DecodingKey::from_ec_der(&key_bytes),
    };
    let mut validation = Validation::new(alg.algorithm());
    if let Some(issuer) = issuer {
        validation.set_issuer(&[issuer]);
    }
    if let Some(audience) = audience {
        validation.set_audience(&[audience]);
    } else {
        // `jsonwebtoken` rejects any `aud` claim unless a validator audience is
        // configured. No audience in the config means "do not check it", so a
        // token that happens to carry `aud` must not be refused for that.
        validation.validate_aud = false;
    }
    parsed.verifier = Some(Verifier { key, validation });
    Ok(parsed)
}

/// Local `401` shared by every rejection path.
fn unauthorized(reason: &str) -> types::HeadersDecision {
    host::log(
        types::LogLevel::Debug,
        &format!("jwt token rejected ({reason})"),
    );
    types::HeadersDecision::Reply(types::LocalReply {
        status: 401,
        headers: vec![
            types::Header {
                name: "content-type".to_string(),
                value: "application/json".to_string(),
            },
            types::Header {
                name: "www-authenticate".to_string(),
                value: "Bearer".to_string(),
            },
        ],
        body: BODY.as_bytes().to_vec(),
    })
}

struct Component;

impl Lifecycle for Component {
    fn start(info: types::PluginInfo) -> Result<(), types::Error> {
        host::log(
            types::LogLevel::Info,
            &format!(
                "jwt-asymmetric {} starting on api {} with {:?}",
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
                "jwt-asymmetric reading `{}` and stripping = {}",
                parsed.header, parsed.strip
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
            let Some(verifier) = config.verifier.as_ref() else {
                // `configure` failed, so the route should not be using this
                // plugin at all; fail closed rather than open.
                return unauthorized("plugin is not configured");
            };

            let Some(token) = req
                .headers
                .iter()
                .find(|header| header.name.eq_ignore_ascii_case(&config.header))
                .and_then(|header| bearer_token(&header.value))
            else {
                return unauthorized("missing bearer token");
            };

            if let Err(err) = jsonwebtoken::decode::<serde_json::Value>(
                token,
                &verifier.key,
                &verifier.validation,
            ) {
                return unauthorized(&err.to_string());
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
