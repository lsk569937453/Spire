//! `io.example.maintenance-mode` — answers 503 with a JSON body while a
//! configured maintenance window is open.
//!
//! The switch is a **time window in the route configuration**, not a value in
//! shared state. That is deliberate: the control plane has no endpoint that
//! writes a plugin's shared data, so a shared-data switch could only ever be
//! flipped by the plugin itself. A window is self-contained, auditable (it lives
//! in the file the operator reviews) and needs nothing but `host.clock`:
//!
//! * "turn maintenance on in 10 minutes for 30 minutes" = add a window;
//! * "turn it on now" = a window that starts in the past and reload
//!   (`POST /reload` or a control-plane config write). Reload re-creates the
//!   instance, which re-runs `configure`; until then the old window stays in
//!   force, exactly like any other configuration change.
//!
//! Configuration (JSON, opaque to the gateway):
//!
//! ```json
//! {
//!   "status": 503,
//!   "retry_after_seconds": 120,
//!   "windows": [
//!     { "start_ms": 1700000000000, "end_ms": 1700003600000,
//!       "message": "scheduled database migration" }
//!   ]
//! }
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

/// A reply body bigger than this is rejected at configure time: the host caps a
/// local reply at `max_output_bytes` (64 KiB by default) and a maintenance body
/// is a line of text, not a page.
const MAX_MESSAGE_BYTES: usize = 512;

#[derive(Debug, Clone, PartialEq)]
struct Window {
    start_ms: u64,
    end_ms: u64,
    message: String,
}

#[derive(Debug, Clone, PartialEq)]
struct Config {
    status: u16,
    retry_after_seconds: u32,
    /// Extra response header, e.g. `x-maintenance: 1`, so clients and proxies
    /// can tell a planned 503 from a broken upstream.
    header: Option<(String, String)>,
    windows: Vec<Window>,
}

thread_local! {
    static CONFIG: RefCell<Option<Config>> = const { RefCell::new(None) };
}

fn valid_header_name(name: &str) -> bool {
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

fn valid_header_value(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|b| b == b'\t' || (0x20..=0x7e).contains(&b))
}

fn parse(raw: &str) -> Result<Config, String> {
    let material = if raw.trim().is_empty() { "{}" } else { raw };
    let doc: serde_json::Value =
        serde_json::from_str(material).map_err(|e| format!("config is not valid JSON: {e}"))?;
    let object = doc
        .as_object()
        .ok_or_else(|| "config must be a JSON object".to_string())?;

    let status = match object.get("status") {
        None => 503,
        Some(value) => {
            let status = value
                .as_u64()
                .ok_or_else(|| "`status` must be an integer".to_string())?;
            // 5xx only: a maintenance window that answers 404 or 200 is a
            // different feature, and answering 2xx with a JSON error body would
            // quietly break clients.
            if !(500..=599).contains(&status) {
                return Err(format!("`status` must be 500..=599, got {status}"));
            }
            status as u16
        }
    };

    let retry_after_seconds = match object.get("retry_after_seconds") {
        None => 60,
        Some(value) => {
            let seconds = value
                .as_u64()
                .ok_or_else(|| "`retry_after_seconds` must be an integer".to_string())?;
            if seconds > 86_400 {
                return Err("`retry_after_seconds` must be at most 86400".to_string());
            }
            seconds as u32
        }
    };

    let header = match object.get("header") {
        None => None,
        Some(value) => {
            let pair = value
                .as_object()
                .ok_or_else(|| "`header` must be an object with `name` and `value`".to_string())?;
            let name = pair
                .get("name")
                .and_then(|name| name.as_str())
                .ok_or_else(|| "`header.name` must be a string".to_string())?
                .to_ascii_lowercase();
            let value = pair
                .get("value")
                .and_then(|value| value.as_str())
                .ok_or_else(|| "`header.value` must be a string".to_string())?;
            if !valid_header_name(&name) {
                return Err(format!("`header.name` `{name}` is not a valid header name"));
            }
            if matches!(
                name.as_str(),
                "content-length" | "transfer-encoding" | "connection" | "retry-after"
            ) {
                return Err(format!("`header.name` `{name}` is set by the plugin itself"));
            }
            if !valid_header_value(value) {
                return Err("`header.value` is not a valid header value".to_string());
            }
            Some((name, value.to_string()))
        }
    };

    let windows_node = object
        .get("windows")
        .ok_or_else(|| "`windows` is required: a maintenance plugin with no window does nothing".to_string())?;
    let windows_array = windows_node
        .as_array()
        .ok_or_else(|| "`windows` must be an array".to_string())?;
    if windows_array.is_empty() {
        return Err("`windows` must not be empty".to_string());
    }
    let mut windows = Vec::with_capacity(windows_array.len());
    for (index, entry) in windows_array.iter().enumerate() {
        let entry = entry
            .as_object()
            .ok_or_else(|| format!("`windows[{index}]` must be an object"))?;
        let start_ms = entry
            .get("start_ms")
            .and_then(|value| value.as_u64())
            .ok_or_else(|| format!("`windows[{index}].start_ms` must be an integer"))?;
        let end_ms = entry
            .get("end_ms")
            .and_then(|value| value.as_u64())
            .ok_or_else(|| format!("`windows[{index}].end_ms` must be an integer"))?;
        if end_ms <= start_ms {
            return Err(format!(
                "`windows[{index}]` ends at or before it starts ({start_ms} -> {end_ms})"
            ));
        }
        let message = match entry.get("message") {
            None => "scheduled maintenance".to_string(),
            Some(value) => {
                let message = value
                    .as_str()
                    .ok_or_else(|| format!("`windows[{index}].message` must be a string"))?;
                if message.len() > MAX_MESSAGE_BYTES {
                    return Err(format!(
                        "`windows[{index}].message` is {} bytes, over {MAX_MESSAGE_BYTES}",
                        message.len()
                    ));
                }
                message.to_string()
            }
        };
        windows.push(Window {
            start_ms,
            end_ms,
            message,
        });
    }

    Ok(Config {
        status,
        retry_after_seconds,
        header,
        windows,
    })
}

/// The window covering `now`, if any. Half-open `[start, end)` so two adjacent
/// windows do not both match at the boundary.
fn active_window<'a>(windows: &'a [Window], now_ms: u64) -> Option<&'a Window> {
    windows
        .iter()
        .find(|window| now_ms >= window.start_ms && now_ms < window.end_ms)
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
                    &format!(
                        "maintenance-mode: {} window(s), answering {}",
                        parsed.windows.len(),
                        parsed.status
                    ),
                );
                CONFIG.with(|c| *c.borrow_mut() = Some(parsed));
                Ok(())
            }
            Err(message) => {
                CONFIG.with(|c| *c.borrow_mut() = None);
                Err(types::Error::Invalid(message))
            }
        }
    }

    fn stop() {}
}

impl RequestFilter for Component {
    fn on_request_headers(req: types::Request) -> types::HeadersDecision {
        let config = CONFIG.with(|c| c.borrow().clone());
        let Some(config) = config else {
            // configure() was rejected; the host applies the route's fail policy.
            return types::HeadersDecision::Proceed(Vec::new());
        };
        let now = host::now_millis();
        let Some(window) = active_window(&config.windows, now) else {
            return types::HeadersDecision::Proceed(Vec::new());
        };

        if let Err(e) = host::incr_counter("blocked", &[], 1) {
            host::log(
                types::LogLevel::Debug,
                &format!("maintenance-mode: cannot count the block: {e:?}"),
            );
        }
        host::log(
            types::LogLevel::Debug,
            &format!(
                "maintenance-mode: {} {} answered {} until {}",
                req.method, req.path, config.status, window.end_ms
            ),
        );

        let body = format!(
            "{{\"error\":\"maintenance\",\"message\":{},\"until_ms\":{}}}",
            json_string(&window.message),
            window.end_ms
        );
        let mut headers = vec![types::Header {
            name: "content-type".to_string(),
            value: "application/json".to_string(),
        }];
        if config.retry_after_seconds > 0 {
            headers.push(types::Header {
                name: "retry-after".to_string(),
                value: config.retry_after_seconds.to_string(),
            });
        }
        if let Some((name, value)) = &config.header {
            headers.push(types::Header {
                name: name.clone(),
                value: value.clone(),
            });
        }
        types::HeadersDecision::Reply(types::LocalReply {
            status: config.status,
            headers,
            body: body.into_bytes(),
        })
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
        types::HeadersDecision::Proceed(Vec::new())
    }

    fn on_response_body(chunk: types::BodyChunk) -> types::BodyDecision {
        types::BodyDecision::Proceed(chunk.bytes)
    }

    fn on_response_trailers(_trailers: Vec<types::Header>) -> types::HeadersDecision {
        types::HeadersDecision::Proceed(Vec::new())
    }

    fn on_log(_summary: types::StreamSummary) {}
}

/// Minimal JSON string escaping for the reply body. `configure` already rejects
/// control characters in a message? It does not — so escape everything that
/// would break the JSON.
fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

export!(Component);

#[cfg(test)]
mod tests {
    use super::*;

    fn config(raw: &str) -> Result<Config, String> {
        parse(raw)
    }

    #[test]
    fn a_window_that_covers_now_blocks() {
        let parsed = config(
            r#"{"windows": [{"start_ms": 100, "end_ms": 200, "message": "db migration"}]}"#,
        )
        .unwrap();
        assert_eq!(active_window(&parsed.windows, 150).unwrap().message, "db migration");
        // Half-open: the end boundary is already outside.
        assert!(active_window(&parsed.windows, 199).is_some());
        assert!(active_window(&parsed.windows, 200).is_none());
        assert!(active_window(&parsed.windows, 99).is_none());
    }

    #[test]
    fn defaults_are_503_with_retry_after() {
        let parsed = config(r#"{"windows": [{"start_ms": 0, "end_ms": 1}]}"#).unwrap();
        assert_eq!(parsed.status, 503);
        assert_eq!(parsed.retry_after_seconds, 60);
        assert!(parsed.header.is_none());
        assert_eq!(parsed.windows[0].message, "scheduled maintenance");
    }

    #[test]
    fn status_must_be_5xx() {
        assert!(config(r#"{"status": 200, "windows": [{"start_ms": 0, "end_ms": 1}]}"#).is_err());
        assert!(config(r#"{"status": 429, "windows": [{"start_ms": 0, "end_ms": 1}]}"#).is_err());
        assert!(config(r#"{"status": 503, "windows": [{"start_ms": 0, "end_ms": 1}]}"#).is_ok());
    }

    #[test]
    fn windows_are_required_and_well_formed() {
        assert!(config("{}").is_err());
        assert!(config(r#"{"windows": []}"#).is_err());
        assert!(config(r#"{"windows": [{"start_ms": 5, "end_ms": 5}]}"#).is_err());
        assert!(config(r#"{"windows": [{"start_ms": 9, "end_ms": 5}]}"#).is_err());
    }

    #[test]
    fn the_extra_header_cannot_shadow_a_plugin_header() {
        assert!(
            config(
                r#"{"header": {"name": "retry-after", "value": "1"},
                    "windows": [{"start_ms": 0, "end_ms": 1}]}"#
            )
            .is_err()
        );
        assert!(
            config(
                r#"{"header": {"name": "x-maintenance", "value": "1"},
                    "windows": [{"start_ms": 0, "end_ms": 1}]}"#
            )
            .is_ok()
        );
    }

    #[test]
    fn reply_body_is_valid_json_even_with_a_hostile_message() {
        let message = r#"quote " and \ and newline
tab	done"#;
        let body = format!(
            "{{\"error\":\"maintenance\",\"message\":{},\"until_ms\":1}}",
            json_string(message)
        );
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["message"].as_str().unwrap(), message);
    }
}
