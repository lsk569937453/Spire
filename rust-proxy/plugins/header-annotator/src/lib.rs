//! A reference Spire plugin: annotates traffic with a configured header, counts
//! requests per status through a guest-defined metric, and keeps a
//! process-wide counter in shared data.
//!
//! It exists to demonstrate every part of the ABI at once, and to be copied:
//!
//! * `lifecycle.start` / `configure` / `stop` — instance setup, and a
//!   configuration parsed **once** instead of per request;
//! * `request-filter.on-request-headers` — read the request, mutate headers;
//! * `response-filter.on-response-headers` — observe the upstream response;
//! * `response-filter.on-log` — per-exchange accounting after the response is
//!   committed;
//! * `host.log`, `host.metrics`, `host.shared-data`, `host.clock` — the four
//!   capabilities, each behind its grant.
//!
//! Configuration format (opaque to the gateway): `key=value` lines, e.g.
//!
//! ```text
//! header=x-served-by
//! value=edge-a
//! ```
//!
//! Build:
//!
//! ```sh
//! rustup target add wasm32-wasip2
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

/// Per-instance state. Kept in a `thread_local` because the component model
/// gives each instance its own linear memory but not a place to hang state;
/// one instance is only ever entered from one thread at a time.
thread_local! {
    static CONFIG: RefCell<Config> = const { RefCell::new(Config::DEFAULT) };
}

#[derive(Clone, Copy)]
struct Config {
    header: &'static str,
    value: &'static str,
}

impl Config {
    const DEFAULT: Config = Config {
        header: "x-served-by",
        value: "spire-plugin",
    };
}

/// Parses `key=value` lines. Unknown keys are ignored, which is what lets a
/// newer gateway pass new keys to an older plugin.
fn parse(config: &str) -> Config {
    let mut parsed = Config::DEFAULT;
    // `&'static str` fields keep the store trivially copyable; the values are
    // bounded and interned by leak-on-first-use. A real plugin would keep
    // `String`s; this one deliberately shows the smallest possible state.
    let mut header: Option<&'static str> = None;
    let mut value: Option<&'static str> = None;
    for line in config.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, val)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "header" => header = Some(Box::leak(val.trim().to_string().into_boxed_str())),
            "value" => value = Some(Box::leak(val.trim().to_string().into_boxed_str())),
            _ => {}
        }
    }
    if let Some(header) = header {
        parsed.header = header;
    }
    if let Some(value) = value {
        parsed.value = value;
    }
    parsed
}

struct Component;

impl Lifecycle for Component {
    fn start(info: types::PluginInfo) -> Result<(), types::Error> {
        host::log(
            types::LogLevel::Info,
            &format!(
                "header-annotator {} starting on api {} with {:?}",
                info.version, info.api_version, info.capabilities
            ),
        );
        Ok(())
    }

    fn configure(config: String) -> Result<(), types::Error> {
        // `configure` runs once per distinct configuration, not per request:
        // this is where parsing belongs.
        let parsed = parse(&config);
        if parsed.header.is_empty() {
            return Err(types::Error::Invalid("header must not be empty".into()));
        }
        host::log(
            types::LogLevel::Debug,
            &format!("annotating with {}: {}", parsed.header, parsed.value),
        );
        CONFIG.with(|c| *c.borrow_mut() = parsed);
        Ok(())
    }

    fn stop() {
        // Flush anything buffered here. Called off the request path.
    }
}

impl RequestFilter for Component {
    fn on_request_headers(req: types::Request) -> types::HeadersDecision {
        let config = CONFIG.with(|c| *c.borrow());
        // Demonstrate shared state: a process-wide request counter, visible to
        // every worker thread because the host keeps it outside the sandbox.
        if let Some(bytes) = host::get_shared_data("header-annotator", "requests") {
            let seen = u64::from_le_bytes(bytes.first_chunk::<8>().copied().unwrap_or([0; 8]));
            host::set_shared_data("header-annotator", "requests", &(seen + 1).to_le_bytes())
                .ok();
        } else {
            host::set_shared_data("header-annotator", "requests", &1u64.to_le_bytes()).ok();
        }

        // Never overwrite what the client sent: `add` keeps both values, and
        // the host applies the ops in order.
        types::HeadersDecision::Proceed(vec![types::HeaderOp::Add(types::Header {
            name: config.header.to_string(),
            value: config.value.to_string(),
        })])
    }

    fn on_request_body(chunk: types::BodyChunk) -> types::BodyDecision {
        // Unchanged pass-through. This route does not enable body filtering, so
        // the host never calls this today; implementing it keeps the component
        // valid for a future `body: stream` route.
        types::BodyDecision::Proceed(chunk.bytes)
    }

    fn on_request_trailers(trailers: Vec<types::Header>) -> types::HeadersDecision {
        let _ = trailers;
        types::HeadersDecision::Proceed(Vec::new())
    }
}

impl ResponseFilter for Component {
    fn on_response_headers(resp: types::Response) -> types::HeadersDecision {
        // Correlate the response with the upstream without touching its status:
        // a plugin that rewrites status is doing the gateway's job badly.
        let upstream_status = resp.status;
        types::HeadersDecision::Proceed(vec![types::HeaderOp::Set(types::Header {
            name: "x-spire-upstream-status".to_string(),
            value: upstream_status.to_string(),
        })])
    }

    fn on_response_body(chunk: types::BodyChunk) -> types::BodyDecision {
        types::BodyDecision::Proceed(chunk.bytes)
    }

    fn on_response_trailers(trailers: Vec<types::Header>) -> types::HeadersDecision {
        let _ = trailers;
        types::HeadersDecision::Proceed(Vec::new())
    }

    fn on_log(summary: types::StreamSummary) {
        // A guest-defined metric: the host names it
        // `spire_plugin_requests_total{plugin="...",status="..."}`.
        let status = summary.status.to_string();
        if let Err(e) = host::incr_counter(
            "requests",
            &[("status".to_string(), status)],
            1,
        ) {
            host::log(
                types::LogLevel::Warn,
                &format!("cannot record metric: {e:?}"),
            );
        }
        host::log(
            types::LogLevel::Debug,
            &format!(
                "{} {} -> {} in {}ms (upstream {}ms)",
                summary.method,
                summary.path,
                summary.status,
                summary.total_millis,
                summary
                    .upstream_millis
                    .map(|ms| ms.to_string())
                    .unwrap_or_else(|| "-".to_string())
            ),
        );
    }
}

export!(Component);
