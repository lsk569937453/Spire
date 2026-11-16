//! Copies request headers onto the response and drops response headers.
//!
//! Configuration is `key=value` lines, parsed once in `configure`. `map` and
//! `drop` may repeat:
//!
//! ```text
//! map=x-request-id:x-request-id
//! map=x-tenant:x-served-tenant
//! drop=server
//! ```
//!
//! Only explicit `map`/`drop` lines do anything; there is no implicit
//! "prefix everything" mode. The relevant hook is `on-response-headers`,
//! which asks the host for the request that produced the response through the
//! `exchange` interface (`none` when the plugin did not see a request).
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
use spire::filter::exchange;
use spire::filter::host;
use spire::filter::types;

// Parsed configuration, written once by `configure` and only read per request.
// One instance is entered from one thread at a time.
thread_local! {
    static CONFIG: RefCell<Config> = const { RefCell::new(Config::EMPTY) };
}

struct Config {
    /// `(request header, response header)` pairs, both lower case.
    map: Vec<(String, String)>,
    /// Response headers to remove, lower case.
    drop: Vec<String>,
}

impl Config {
    const EMPTY: Config = Config {
        map: Vec::new(),
        drop: Vec::new(),
    };
}

/// Parses the `key=value` configuration. Unknown keys are ignored so a newer
/// gateway can pass new keys to an older plugin.
fn parse(config: &str) -> Result<Config, types::Error> {
    let mut parsed = Config::EMPTY;
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
            "map" => {
                let Some((from, to)) = value.split_once(':') else {
                    return Err(types::Error::Invalid(format!(
                        "map `{value}` must be `<request-header>:<response-header>`"
                    )));
                };
                let (from, to) = (from.trim(), to.trim());
                if from.is_empty() || to.is_empty() {
                    return Err(types::Error::Invalid(format!(
                        "map `{value}` must name both headers"
                    )));
                }
                parsed
                    .map
                    .push((from.to_ascii_lowercase(), to.to_ascii_lowercase()));
            }
            "drop" => {
                if value.is_empty() {
                    return Err(types::Error::Invalid(
                        "drop must name a response header".into(),
                    ));
                }
                parsed.drop.push(value.to_ascii_lowercase());
            }
            _ => {}
        }
    }
    Ok(parsed)
}

struct Component;

impl Lifecycle for Component {
    fn start(info: types::PluginInfo) -> Result<(), types::Error> {
        host::log(
            types::LogLevel::Info,
            &format!(
                "header-to-response {} starting on api {} with {:?}",
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
                "header-to-response maps {} header(s) and drops {}",
                parsed.map.len(),
                parsed.drop.len()
            ),
        );
        CONFIG.with(|slot| *slot.borrow_mut() = parsed);
        Ok(())
    }

    fn stop() {}
}

impl RequestFilter for Component {
    fn on_request_headers(req: types::Request) -> types::HeadersDecision {
        let _ = req;
        types::HeadersDecision::Proceed(Vec::new())
    }

    fn on_request_body(chunk: types::BodyChunk) -> types::BodyDecision {
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
        // No request in scope (a transport error, for example): nothing to
        // copy, and the response is left untouched.
        let Some(req) = exchange::current_request() else {
            return types::HeadersDecision::Proceed(Vec::new());
        };

        let mut ops = Vec::new();
        CONFIG.with(|slot| {
            let config = slot.borrow();
            for (from, to) in &config.map {
                // Header names arrive lower cased, so a plain comparison is a
                // case-insensitive one; values are copied verbatim.
                let Some(header) = req
                    .headers
                    .iter()
                    .find(|header| header.name.eq_ignore_ascii_case(from))
                else {
                    continue;
                };
                ops.push(types::HeaderOp::Set(types::Header {
                    name: to.clone(),
                    value: header.value.clone(),
                }));
            }
            for name in &config.drop {
                ops.push(types::HeaderOp::Remove(name.clone()));
            }
        });
        types::HeadersDecision::Proceed(ops)
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
    fn parses_repeated_map_and_drop() {
        let config = parse(
            "map=x-request-id:x-request-id\nmap=X-Tenant:x-served-tenant\ndrop=Server\nprefix=x-req-\n",
        )
        .unwrap();
        assert_eq!(
            config.map,
            vec![
                ("x-request-id".to_string(), "x-request-id".to_string()),
                ("x-tenant".to_string(), "x-served-tenant".to_string()),
            ]
        );
        assert_eq!(config.drop, vec!["server".to_string()]);
    }

    #[test]
    fn rejects_malformed_map_and_empty_drop() {
        assert!(parse("map=missing-colon").is_err());
        assert!(parse("map=:x").is_err());
        assert!(parse("drop= ").is_err());
    }
}
