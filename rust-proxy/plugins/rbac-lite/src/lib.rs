//! Lightweight role-based access control.
//!
//! Evaluates ordered method/path rules and answers `403` for denied traffic.
//! The first matching rule wins; when no rule matches the configured default
//! decides.
//!
//! Configuration is `key=value` lines (opaque to the gateway), e.g.
//!
//! ```text
//! default=allow
//! allow=GET:/api/v1/*
//! deny=*:/admin
//! ```
//!
//! A method of `*` matches every method, and a path ending in `*` is a prefix
//! match; any other path is compared exactly.
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

const ERROR_BODY: &[u8] = br#"{"error":"forbidden"}"#;

thread_local! {
    static CONFIG: RefCell<Config> = RefCell::new(Config::defaults());
}

#[derive(Clone)]
struct Rule {
    allow: bool,
    /// Uppercased method, or `*` for any method.
    method: String,
    /// Path, with a trailing `*` meaning prefix match.
    path: String,
}

#[derive(Clone)]
struct Config {
    default_allow: bool,
    rules: Vec<Rule>,
}

impl Config {
    fn defaults() -> Config {
        Config {
            default_allow: true,
            rules: Vec::new(),
        }
    }
}

/// Parses `key=value` lines. Unknown keys are ignored so a newer gateway can
/// pass extra keys to an older plugin. Rules keep their configuration order.
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
            "default" => match value.trim().to_ascii_lowercase().as_str() {
                "allow" => parsed.default_allow = true,
                "deny" => parsed.default_allow = false,
                other => host::log(
                    types::LogLevel::Warn,
                    &format!("rbac-lite: unknown default '{other}', keeping allow"),
                ),
            },
            "allow" => push_rule(&mut parsed.rules, true, value.trim()),
            "deny" => push_rule(&mut parsed.rules, false, value.trim()),
            _ => {}
        }
    }
    parsed
}

fn push_rule(rules: &mut Vec<Rule>, allow: bool, value: &str) {
    let Some((method, path)) = value.split_once(':') else {
        host::log(
            types::LogLevel::Warn,
            &format!("rbac-lite: rule '{value}' is not <METHOD>:<path>, ignored"),
        );
        return;
    };
    let method = method.trim().to_ascii_uppercase();
    let path = path.trim();
    if method.is_empty() || path.is_empty() {
        host::log(
            types::LogLevel::Warn,
            &format!("rbac-lite: rule '{value}' has an empty method or path, ignored"),
        );
        return;
    }
    rules.push(Rule {
        allow,
        method,
        path: path.to_string(),
    });
}

fn rule_matches(rule: &Rule, method: &str, path: &str) -> bool {
    if rule.method != "*" && !rule.method.eq_ignore_ascii_case(method) {
        return false;
    }
    match rule.path.strip_suffix('*') {
        Some(prefix) => path.starts_with(prefix),
        None => path == rule.path,
    }
}

fn record(outcome: &str) {
    if let Err(error) = host::incr_counter(
        "requests",
        &[("outcome".to_string(), outcome.to_string())],
        1,
    ) {
        host::log(
            types::LogLevel::Warn,
            &format!("rbac-lite: cannot record metric: {error:?}"),
        );
    }
}

fn reject() -> types::HeadersDecision {
    types::HeadersDecision::Reply(types::LocalReply {
        status: 403,
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
                "rbac-lite {} starting on api {}",
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
                "rbac-lite: {} rule(s), default={}",
                parsed.rules.len(),
                if parsed.default_allow {
                    "allow"
                } else {
                    "deny"
                }
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
            let mut allowed = config.default_allow;
            for rule in &config.rules {
                if rule_matches(rule, &req.method, &req.path) {
                    allowed = rule.allow;
                    break;
                }
            }
            if allowed {
                record("allowed");
                types::HeadersDecision::Proceed(Vec::new())
            } else {
                host::log(
                    types::LogLevel::Debug,
                    &format!("rbac-lite: denied {} {}", req.method, req.path),
                );
                record("denied");
                reject()
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
