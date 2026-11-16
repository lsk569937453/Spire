//! The WASM data-plane plugin system: discovery, compilation, instance pools
//! and the four hooks the request pipeline calls.
//!
//! # Shape of the system
//!
//! ```text
//!   plugins dir ──scan──> PluginPackage (plugin.toml validated)
//!                                │
//!                                ├─ wasmtime::component::Component (compiled once,
//!                                │   process-wide, never on the request path)
//!                                ▼
//!   route config ──> WasmPluginConfig ──> per-thread instance pool
//!                                              │
//!                                              ├─ lifecycle.start / configure
//!                                              ├─ request_filter.on-request-headers
//!                                              └─ response_filter.on-response-headers / on-log
//! ```
//!
//! Everything here is behind the `wasm` Cargo feature. The configuration
//! *surface* (`kind: wasm` in a route) is always compiled, so a build without
//! the feature rejects such a config loudly instead of ignoring it.
//!
//! # Design rules that are not negotiable
//!
//! * A guest never holds a reference to gateway state: it gets copies of the
//!   request/response views and returns decisions the host applies.
//! * No WASI: the linker exposes the `spire:filter/host` functions and nothing
//!   else.
//! * Compilation and `configure` happen at configuration load; a request only
//!   ever instantiates a new store the first time a thread touches a plugin.
//! * Every callback is bounded by fuel *and* wall clock; a trapped instance is
//!   discarded, never reused.

pub mod bindings;
pub mod convert;
pub mod engine;
pub mod manifest;
pub mod outbound;
pub mod pool;
pub mod state;

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::RwLock;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Instant;

use wasmtime::component::Component;

use crate::middleware::middlewares::Denial;
use crate::middleware::middlewares::ExchangeSummary;
use crate::middleware::middlewares::MiddleWares;
use crate::middleware::middlewares::RequestPhase;
use crate::middleware::middlewares::ResponsePhase;
use crate::middleware::wasm_plugin_config::FailPolicy;
use crate::middleware::wasm_plugin_config::WasmPluginConfig;
use crate::vojo::app_config::AppConfig;
use crate::vojo::app_error::AppError;
use bindings::spire::filter::types as wit;
use state::PluginLimits;
use state::StoreData;

/// A compiled, ready-to-instantiate plugin.
pub struct CompiledPlugin {
    pub manifest: manifest::PluginManifest,
    pub component: Component,
    /// Bumped on every reload. Instances are keyed by it, so a reload retires
    /// every pooled instance without touching a running request.
    pub generation: u64,
    pub granted: Arc<Vec<String>>,
}

static ENGINE: OnceLock<engine::PluginEngine> = OnceLock::new();
static PLUGINS: RwLock<Vec<Arc<CompiledPlugin>>> = RwLock::new(Vec::new());
static FAILED: RwLock<Option<HashMap<String, String>>> = RwLock::new(None);
static DIR: OnceLock<PathBuf> = OnceLock::new();
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// `~/.spire/plugins`, next to the certificates under `~/.spire/domains`.
pub fn default_dir() -> PathBuf {
    match home::home_dir() {
        Some(home) => home.join(".spire").join("plugins"),
        None => PathBuf::from("plugins"),
    }
}

/// Starts the plugin system: creates the engine once, then compiles every
/// package under `dir`.
///
/// A package that fails to load does not stop the gateway (an unused broken
/// plugin must not take traffic down) but it does become *permanently
/// reported*: a route that references it fails validation with the recorded
/// reason instead of a generic "plugin not found".
pub fn init(dir: Option<PathBuf>) -> Result<(), AppError> {
    let dir = dir.unwrap_or_else(default_dir);
    if let Some(existing) = DIR.get()
        && existing != &dir
    {
        debug!(
            "wasm: plugins directory is already {} ; ignoring {}",
            existing.display(),
            dir.display()
        );
        return reload();
    }
    let _ = DIR.set(dir.clone());

    if ENGINE.get().is_none() {
        let plugin_engine = engine::PluginEngine::new()?;
        let _ = ENGINE.set(plugin_engine);
    }
    reload()
}

/// Re-scans the plugins directory and recompiles anything that changed.
///
/// Called at startup and after `POST /reload` swaps the configuration. The old
/// `Arc<CompiledPlugin>` values stay alive for requests already in flight; the
/// new generation is what new requests resolve.
pub fn reload() -> Result<(), AppError> {
    let Some(dir) = DIR.get() else {
        return Err(AppError(
            "wasm: plugin system was not initialised".to_string(),
        ));
    };
    let Some(plugin_engine) = ENGINE.get() else {
        return Err(AppError(
            "wasm: plugin engine was not initialised".to_string(),
        ));
    };

    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let mut compiled: Vec<Arc<CompiledPlugin>> = Vec::new();
    let mut failed: HashMap<String, String> = HashMap::new();

    let packages = scan_packages(dir)?;
    for package in packages {
        let id = package.manifest.id.clone();
        match plugin_engine.compile(&package.wasm_path) {
            Ok(component) => {
                info!(
                    plugin = id.as_str(),
                    version = package.manifest.version.as_str(),
                    "wasm: loaded plugin"
                );
                compiled.push(Arc::new(CompiledPlugin {
                    granted: Arc::new(package.manifest.capabilities.granted()),
                    manifest: package.manifest,
                    component,
                    generation,
                }));
            }
            Err(e) => {
                error!("wasm: {e}");
                failed.insert(id, e.0);
            }
        }
    }

    let current: HashMap<String, String> = compiled
        .iter()
        .map(|plugin| (plugin.manifest.id.clone(), plugin.generation.to_string()))
        .collect();
    *PLUGINS.write()? = compiled;
    *FAILED.write()? = Some(failed);

    // Retire this thread's stale instances immediately; other threads drop
    // theirs lazily through the pool's size cap (they cannot be reached from
    // here without a channel into every worker).
    pool::retain_generation(&current);

    Ok(())
}

fn scan_packages(dir: &Path) -> Result<Vec<manifest::PluginPackage>, AppError> {
    if !dir.is_dir() {
        debug!(
            "wasm: plugins directory {} does not exist, no plugins loaded",
            dir.display()
        );
        return Ok(Vec::new());
    }
    let mut packages = Vec::new();
    let entries = std::fs::read_dir(dir)
        .map_err(|e| AppError(format!("wasm: cannot read {}: {e}", dir.display())))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() || !path.join("plugin.toml").is_file() {
            continue;
        }
        match manifest::PluginPackage::load(&path) {
            Ok(package) => packages.push(package),
            Err(e) => error!("wasm: {e}"),
        }
    }
    packages.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    Ok(packages)
}

/// Looks up a compiled plugin by manifest id.
pub fn resolve(id: &str) -> Result<Arc<CompiledPlugin>, AppError> {
    if let Ok(plugins) = PLUGINS.read()
        && let Some(plugin) = plugins.iter().find(|p| p.manifest.id == id)
    {
        return Ok(plugin.clone());
    }
    if let Ok(failed) = FAILED.read()
        && let Some(reason) = failed.as_ref().and_then(|f| f.get(id))
    {
        return Err(AppError(format!("wasm plugin `{id}`: {reason}")));
    }
    let known: Vec<String> = PLUGINS
        .read()
        .map(|p| p.iter().map(|p| p.manifest.id.clone()).collect())
        .unwrap_or_default();
    let directory = match DIR.get() {
        Some(dir) => dir.display().to_string(),
        None => format!("{} (not scanned in this process)", default_dir().display()),
    };
    Err(AppError(format!(
        "wasm plugin `{id}` is not loaded (loaded: [{}]; directory: {directory})",
        known.join(", ")
    )))
}

/// Every plugin id currently loaded (for logs and `spire validate`).
pub fn loaded_ids() -> Vec<String> {
    PLUGINS
        .read()
        .map(|p| p.iter().map(|p| p.manifest.id.clone()).collect())
        .unwrap_or_default()
}

/// Fails when a route references a plugin that is not loaded, or carries
/// budgets the host refuses. Used by config load, `spire validate` and the
/// SIGHUP reload so the same rule applies everywhere.
pub fn validate_references(config: &AppConfig) -> Result<(), AppError> {
    for service in config.api_service_config.values() {
        for route in &service.route_configs {
            let Some(middlewares) = &route.middlewares else {
                continue;
            };
            for middleware in middlewares {
                if let MiddleWares::Wasm(cfg) = middleware {
                    cfg.validate().map_err(|e| {
                        AppError(format!(
                            "route {}: {e}",
                            if route.route_id.is_empty() {
                                "<unnamed>"
                            } else {
                                route.route_id.as_str()
                            }
                        ))
                    })?;
                    if ENGINE.get().is_some() {
                        resolve(&cfg.plugin)?;
                    }
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Hooks called by the middleware layer
// ---------------------------------------------------------------------------

/// Runs `request_filter.on-request-headers` for one plugin.
///
/// * `Ok(None)` — the plugin proceeded; its header mutations are applied to
///   `phase.headers` in place.
/// * `Ok(Some(denial))` — the plugin produced a local reply (or the fail policy
///   did), which the pipeline returns to the client as-is.
/// * `Err(_)` — the plugin is misconfigured or the host is broken; the request
///   is answered with 502 by the generic error path.
pub fn request_phase(
    cfg: &WasmPluginConfig,
    phase: &mut RequestPhase<'_>,
) -> Result<Option<Denial>, AppError> {
    let plugin = resolve(&cfg.plugin)?;
    let key = pool::slot_key(&plugin.manifest.id, plugin.generation, &cfg.instance_key());
    let slot = pool::acquire(&key, || create_slot(&plugin, cfg, &key))?;

    let client_ip = phase.client_addr.ip().to_string();
    let view = convert::RequestView {
        method: phase.method.as_str(),
        scheme: phase.scheme.as_str(),
        authority: phase.authority.as_str(),
        path: phase.path.as_str(),
        query: phase.query.as_str(),
        headers: phase.headers,
        client_ip: &client_ip,
        route_id: phase.route_id,
        listener_port: phase.listener_port.max(0) as u32,
        request_id: phase.request_id,
    };
    let request = convert::request_to_wit(view);

    let mut guard = slot.borrow_mut();
    let slot = &mut *guard;
    // Clear the previous exchange's request before the guest can see it; the
    // response phase installs this exchange's own snapshot.
    slot.store.data_mut().current_request = None;
    let max_output = slot.store.data().limits.max_output_bytes as u64;
    engine::arm_budget(&mut slot.store)?;
    let started = Instant::now();
    let result = slot
        .plugin
        .spire_filter_request_filter()
        .call_on_request_headers(&mut slot.store, &request);
    let elapsed = started.elapsed();
    let phase_name = "request_headers";
    match result {
        Ok(decision) => {
            crate::monitor::prometheus_exporter::metrics::observe_wasm_call(
                &plugin.manifest.id,
                phase_name,
                elapsed.as_secs_f64(),
                "ok",
            );
            match decision {
                wit::HeadersDecision::Proceed(ops) => {
                    convert::apply_header_ops(phase.headers, ops).map_err(|e| {
                        AppError(format!("wasm plugin `{}`: {e}", plugin.manifest.id))
                    })?;
                    Ok(None)
                }
                wit::HeadersDecision::Reply(reply) => Ok(Some(
                    convert::local_reply_to_denial(reply, max_output).map_err(|e| {
                        AppError(format!("wasm plugin `{}`: {e}", plugin.manifest.id))
                    })?,
                )),
            }
        }
        Err(e) => {
            let (reason, message) = engine::classify_error(&e);
            drop(guard);
            // The store may be inconsistent after a trap: throw it away so the
            // next request gets a clean instance.
            pool::discard(&key);
            crate::monitor::prometheus_exporter::metrics::observe_wasm_call(
                &plugin.manifest.id,
                phase_name,
                elapsed.as_secs_f64(),
                reason,
            );
            fail(cfg, &plugin.manifest.id, phase_name, reason, &message)
        }
    }
}

/// Runs `response_filter.on-response-headers` for one plugin.
pub fn response_phase(
    cfg: &WasmPluginConfig,
    phase: &mut ResponsePhase<'_>,
) -> Result<Option<Denial>, AppError> {
    let plugin = resolve(&cfg.plugin)?;
    let key = pool::slot_key(&plugin.manifest.id, plugin.generation, &cfg.instance_key());
    let slot = pool::acquire(&key, || create_slot(&plugin, cfg, &key))?;

    let status = phase.response.status();
    let response = convert::response_to_wit(status, phase.response.headers());

    // Wrap the current response body so the plugin can observe it; the body
    // itself is only handed over in `body: buffered|stream` mode (M2).
    let mut guard = slot.borrow_mut();
    let slot = &mut *guard;
    slot.store.data_mut().current_request = phase.request.map(convert::snapshot_to_wit);
    let max_output = slot.store.data().limits.max_output_bytes as u64;
    engine::arm_budget(&mut slot.store)?;
    let started = Instant::now();
    let result = slot
        .plugin
        .spire_filter_response_filter()
        .call_on_response_headers(&mut slot.store, &response);
    let elapsed = started.elapsed();
    let phase_name = "response_headers";
    match result {
        Ok(decision) => {
            crate::monitor::prometheus_exporter::metrics::observe_wasm_call(
                &plugin.manifest.id,
                phase_name,
                elapsed.as_secs_f64(),
                "ok",
            );
            match decision {
                wit::HeadersDecision::Proceed(ops) => {
                    let headers = phase.response.headers_mut();
                    convert::apply_header_ops(headers, ops).map_err(|e| {
                        AppError(format!("wasm plugin `{}`: {e}", plugin.manifest.id))
                    })?;
                    Ok(None)
                }
                wit::HeadersDecision::Reply(reply) => Ok(Some(
                    convert::local_reply_to_denial(reply, max_output).map_err(|e| {
                        AppError(format!("wasm plugin `{}`: {e}", plugin.manifest.id))
                    })?,
                )),
            }
        }
        Err(e) => {
            let (reason, message) = engine::classify_error(&e);
            debug!(
                plugin = plugin.manifest.id.as_str(),
                route = phase.route_id,
                request_id = phase.request_id,
                "wasm: response phase failed"
            );
            drop(guard);
            pool::discard(&key);
            crate::monitor::prometheus_exporter::metrics::observe_wasm_call(
                &plugin.manifest.id,
                phase_name,
                elapsed.as_secs_f64(),
                reason,
            );
            fail(cfg, &plugin.manifest.id, phase_name, reason, &message)
        }
    }
}

/// Runs `response_filter.on-log`. Errors are logged and swallowed: the client's
/// response has already been committed by the time this runs.
pub fn log_phase(cfg: &WasmPluginConfig, summary: &ExchangeSummary<'_>) {
    if let Err(e) = try_log_phase(cfg, summary) {
        debug!("wasm: on-log skipped: {e}");
    }
}

/// Same as [`log_phase`], but surfaces the reason. Tests use it; the pipeline
/// must not, because nothing it could do about the error is useful.
pub fn try_log_phase(
    cfg: &WasmPluginConfig,
    summary: &ExchangeSummary<'_>,
) -> Result<(), AppError> {
    let plugin = resolve(&cfg.plugin)?;
    let key = pool::slot_key(&plugin.manifest.id, plugin.generation, &cfg.instance_key());
    let slot = pool::acquire(&key, || create_slot(&plugin, cfg, &key))?;
    let summary = wit::StreamSummary {
        request_id: summary.request_id.to_string(),
        route_id: summary.route_id.to_string(),
        method: summary.method.to_string(),
        path: summary.path.to_string(),
        status: summary.status,
        outcome: if summary.local {
            wit::Outcome::Local
        } else if summary.status >= 500 {
            wit::Outcome::ServerError
        } else if summary.status >= 400 {
            wit::Outcome::ClientError
        } else {
            wit::Outcome::Success
        },
        total_millis: summary.total_millis,
        upstream_millis: summary.upstream_millis,
        request_bytes: summary.request_bytes,
        response_bytes: summary.response_bytes,
    };
    let mut guard = slot.borrow_mut();
    let slot = &mut *guard;
    engine::arm_budget(&mut slot.store)?;
    let result = slot
        .plugin
        .spire_filter_response_filter()
        .call_on_log(&mut slot.store, &summary);
    if let Err(e) = result {
        let (reason, message) = engine::classify_error(&e);
        drop(guard);
        pool::discard(&key);
        crate::monitor::prometheus_exporter::metrics::record_wasm_trap(&plugin.manifest.id, reason);
        return Err(AppError(format!(
            "wasm plugin `{}`: on-log {reason}: {message}",
            plugin.manifest.id
        )));
    }
    Ok(())
}

/// Applies the route's fail policy to a guest failure.
fn fail(
    cfg: &WasmPluginConfig,
    plugin_id: &str,
    phase: &str,
    reason: &str,
    message: &str,
) -> Result<Option<Denial>, AppError> {
    crate::monitor::prometheus_exporter::metrics::record_wasm_trap(plugin_id, reason);
    match cfg.fail {
        FailPolicy::Open => {
            warn!(
                plugin = plugin_id,
                phase, reason, "wasm: plugin failed, continuing (fail: open): {message}"
            );
            Ok(None)
        }
        FailPolicy::Closed => {
            error!(
                plugin = plugin_id,
                phase,
                reason,
                "wasm: plugin failed, refusing the request (fail: closed): {message}"
            );
            let mut headers = http::HeaderMap::new();
            headers.insert(
                http::header::CONTENT_TYPE,
                http::HeaderValue::from_static("application/json"),
            );
            headers.insert(
                http::header::RETRY_AFTER,
                http::HeaderValue::from_static("1"),
            );
            Ok(Some(Denial {
                status: http::StatusCode::SERVICE_UNAVAILABLE,
                headers,
                body: format!(
                    "{{\"error\":\"plugin unavailable\",\"plugin\":\"{}\"}}",
                    plugin_id.replace('"', "")
                ),
            }))
        }
    }
}

fn create_slot(
    plugin: &Arc<CompiledPlugin>,
    cfg: &WasmPluginConfig,
    _key: &str,
) -> Result<pool::Slot, AppError> {
    let Some(plugin_engine) = ENGINE.get() else {
        return Err(AppError(
            "wasm: plugin engine was not initialised".to_string(),
        ));
    };
    let data = StoreData::new(
        plugin.manifest.id.clone(),
        plugin.granted.clone(),
        Arc::from(cfg.config.as_deref().unwrap_or("")),
        PluginLimits::from_config(cfg),
        state::shared_data().clone(),
        state::plugin_metrics().clone(),
    );
    let (mut store, guest) = plugin_engine.instantiate(&plugin.component, data)?;

    let info = wit::PluginInfo {
        id: plugin.manifest.id.clone(),
        version: plugin.manifest.version.clone(),
        api_version: format!("{}.{}.0", manifest::API_MAJOR, manifest::API_MINOR),
        capabilities: plugin.granted.as_ref().clone(),
    };

    engine::arm_budget(&mut store)?;
    // `call_*` returns `wasmtime::Result<Result<(), types::Error>>`: the outer
    // error is a trap, the inner one is the guest's own `result<_, error>`.
    // Both must be handled — ignoring the inner one silently accepted a plugin
    // whose `start()` said no.
    guest
        .spire_filter_lifecycle()
        .call_start(&mut store, &info)
        .map_err(|e| {
            AppError(format!(
                "wasm plugin `{}`: start() trapped: {e}",
                plugin.manifest.id
            ))
        })?
        .map_err(|e| {
            AppError(format!(
                "wasm plugin `{}`: start() refused to activate: {e:?}",
                plugin.manifest.id
            ))
        })?;

    let config_blob = data_config(cfg);
    engine::arm_budget(&mut store)?;
    guest
        .spire_filter_lifecycle()
        .call_configure(&mut store, &config_blob)
        .map_err(|e| {
            AppError(format!(
                "wasm plugin `{}`: configure() trapped: {e}",
                plugin.manifest.id
            ))
        })?
        .map_err(|e| {
            AppError(format!(
                "wasm plugin `{}`: configure() rejected its configuration: {e:?}",
                plugin.manifest.id
            ))
        })?;

    Ok(pool::Slot::new(store, guest))
}

fn data_config(cfg: &WasmPluginConfig) -> String {
    cfg.config.clone().unwrap_or_default()
}

/// Signals every live instance on this thread to stop, and clears the pool.
/// Called on shutdown.
pub fn shutdown() {
    pool::retain_generation(&HashMap::new());
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http::StatusCode;
    use http_body_util::combinators::BoxBody;
    use std::net::SocketAddr;

    /// Copies every packaged plugin that has a built component into a temp
    /// plugins directory. Returns `None` when no component has been built, so
    /// `cargo test` still works on a machine without the `wasm32-wasip2` target.
    ///
    /// A package without `dist/plugin.wasm` is skipped rather than failed: the
    /// committed components are optional for a local checkout, but every
    /// component that *is* present must load.
    fn install_packaged_plugins() -> Option<PathBuf> {
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("plugins");
        let reference = source.join("header-annotator");
        if !reference.join("dist/plugin.wasm").is_file() {
            eprintln!(
                "skipping: build plugins/header-annotator first \
                 (rustup target add wasm32-wasip2 && cargo build --release --target wasm32-wasip2)"
            );
            return None;
        }
        let dir = std::env::temp_dir().join(format!(
            "spire-plugins-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        for entry in std::fs::read_dir(&source).ok()?.flatten() {
            let package_source = entry.path();
            if !package_source.is_dir() || !package_source.join("dist/plugin.wasm").is_file() {
                continue;
            }
            let manifest_text =
                std::fs::read_to_string(package_source.join("plugin.toml")).unwrap();
            let manifest: manifest::PluginManifest = toml::from_str(&manifest_text).unwrap();
            let package = dir.join(&manifest.id);
            std::fs::create_dir_all(package.join("dist")).unwrap();
            std::fs::copy(
                package_source.join("plugin.toml"),
                package.join("plugin.toml"),
            )
            .unwrap();
            std::fs::copy(
                package_source.join("dist/plugin.wasm"),
                package.join("dist/plugin.wasm"),
            )
            .unwrap();
        }
        Some(dir)
    }

    /// Every copied package must appear in the registry: a package that fails
    /// to compile or validate becomes a `resolve()` error, and this test is the
    /// guard that it does not go unnoticed.
    #[test]
    #[serial_test::serial]
    fn every_packaged_plugin_loads() {
        let Some(dir) = install_packaged_plugins() else {
            return;
        };
        init(Some(dir)).expect("plugin system should initialise");
        let loaded = loaded_ids();
        let mut checked = 0;
        for entry in std::fs::read_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("plugins"))
            .unwrap()
            .flatten()
        {
            let package = entry.path();
            if !package.is_dir() || !package.join("dist/plugin.wasm").is_file() {
                continue;
            }
            let text = std::fs::read_to_string(package.join("plugin.toml")).unwrap();
            let manifest: manifest::PluginManifest = toml::from_str(&text).unwrap();
            assert!(
                loaded.contains(&manifest.id),
                "{} did not load; loaded = {loaded:?}",
                manifest.id
            );
            checked += 1;
        }
        assert!(
            checked >= 1,
            "at least the reference plugin should be present"
        );
    }

    fn config(blob: &str) -> WasmPluginConfig {
        let yaml = format!(
            "plugin: io.example.header-annotator\nconfig: |\n{}",
            blob.lines()
                .map(|l| format!("  {l}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        serde_yaml::from_str(&yaml).unwrap()
    }

    /// The whole point of the module: a real component, compiled by a real
    /// guest toolchain, running through the real host path.
    #[test]
    #[serial_test::serial]
    fn reference_plugin_round_trip() {
        let Some(dir) = install_packaged_plugins() else {
            return;
        };
        init(Some(dir)).expect("plugin system should initialise");
        assert!(loaded_ids().contains(&"io.example.header-annotator".to_string()));
        // The host metrics are registered by the same bootstrap the server
        // calls; without it the `spire_wasm_*` families do not exist.
        crate::monitor::prometheus_exporter::metrics::init().expect("metrics should register");

        let cfg = config("header=x-test\nvalue=abc");

        // --- request phase: the plugin adds a header -------------------------
        let client: SocketAddr = "10.0.0.1:1234".parse().unwrap();
        let mut headers = http::HeaderMap::new();
        headers.insert("host", "example.com".parse().unwrap());
        let mut phase = RequestPhase {
            method: "GET".to_string(),
            scheme: "http".to_string(),
            authority: "example.com".to_string(),
            path: "/api/x".to_string(),
            query: "a=1".to_string(),
            headers: &mut headers,
            client_addr: client,
            route_id: "r1",
            listener_port: 8086,
            request_id: "req-1",
        };
        let denial = request_phase(&cfg, &mut phase).expect("request phase should not error");
        assert!(denial.is_none(), "the plugin must not short-circuit");
        assert!(
            pool::live_slots() >= 1,
            "the instance should be pooled on this thread"
        );
        assert_eq!(
            phase.headers.get("x-test").map(|v| v.to_str().unwrap()),
            Some("abc"),
            "the configured header should have been added"
        );

        // --- response phase: the plugin observes the upstream status ---------
        let mut response = http::Response::builder()
            .status(StatusCode::OK)
            .body(BoxBody::<Bytes, AppError>::default())
            .unwrap();
        {
            // Scoped so the mutable borrow of `response` ends before the
            // assertions below read it again.
            let mut rphase = ResponsePhase {
                route_id: "r1",
                request_id: "req-1",
                request: None,
                response: &mut response,
            };
            assert!(response_phase(&cfg, &mut rphase).unwrap().is_none());
        }
        assert_eq!(
            response
                .headers()
                .get("x-spire-upstream-status")
                .map(|v| v.to_str().unwrap()),
            Some("200")
        );

        // --- log phase + guest metrics + shared data -------------------------
        let summary = ExchangeSummary {
            request_id: "req-1",
            route_id: "r1",
            method: "GET",
            path: "/api/x",
            status: 200,
            local: false,
            total_millis: 3,
            upstream_millis: Some(2),
            request_bytes: None,
            response_bytes: None,
        };
        try_log_phase(&cfg, &summary).expect("on-log should succeed");
        let seen = state::shared_data()
            .get("header-annotator", "requests")
            .expect("the plugin should have written shared data");
        assert_eq!(
            u64::from_le_bytes(seen.first_chunk::<8>().copied().unwrap()),
            1
        );

        let families: Vec<String> = prometheus::gather()
            .iter()
            .map(|family| family.name().to_string())
            .collect();
        assert!(
            families
                .iter()
                .any(|name| name == "spire_plugin_io_example_header_annotator_requests_total"),
            "the guest-defined counter should be exported; families = {families:?}"
        );
        assert!(
            families.iter().any(|name| name == "spire_wasm_calls_total"),
            "host-side call metrics should be exported; families = {families:?}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn unknown_plugin_is_reported_with_its_directory() {
        let Some(dir) = install_packaged_plugins() else {
            return;
        };
        init(Some(dir)).expect("plugin system should initialise");
        match resolve("io.example.not-installed") {
            Ok(_) => panic!("an unknown plugin must not resolve"),
            Err(err) => assert!(err.0.contains("io.example.not-installed"), "{}", err.0),
        }
    }

    /// The plugins under `plugins/` carry a copy of the WIT so they can be
    /// built outside this repository. A copy that silently drifts from the
    /// host's definition is worse than no copy: the guest compiles and then
    /// fails at instantiation. This test is the guard.
    #[test]
    fn plugin_wit_matches_host_wit() {
        let host = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("wit/spire-filter.wit");
        let host_text = match std::fs::read_to_string(&host) {
            Ok(text) => text,
            Err(_) => return,
        };
        let plugins_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("plugins");
        let Ok(entries) = std::fs::read_dir(&plugins_dir) else {
            return;
        };
        for entry in entries.flatten() {
            let copy = entry.path().join("wit/spire-filter.wit");
            if !copy.is_file() {
                continue;
            }
            let copied = std::fs::read_to_string(&copy).unwrap();
            assert_eq!(
                copied,
                host_text,
                "{} has drifted from {}; copy the host file over it",
                copy.display(),
                host.display()
            );
        }
    }

    #[test]
    fn a_broken_manifest_is_rejected_before_compilation() {
        let dir = std::env::temp_dir().join(format!("spire-broken-plugin-{}", std::process::id()));
        let package = dir.join("bad");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(
            package.join("plugin.toml"),
            "id = \"Bad Id\"\nname = \"x\"\nversion = \"1\"\nwasm = \"p.wasm\"\n",
        )
        .unwrap();
        let err = manifest::PluginPackage::load(&package).unwrap_err();
        assert!(
            err.0.contains("must match") || err.0.contains("semver"),
            "{}",
            err.0
        );
    }
}
