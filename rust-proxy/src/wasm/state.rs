//! Per-instance host state and the implementation of the `host` WIT interface.
//!
//! `StoreData` is the only thing a guest can reach through an import: it owns
//! the capability grants, the route's configuration blob, the shared-state
//! handle, the per-call budget and the per-instance resource limits. A guest
//! never gets a pointer to gateway structures, which is the whole point of the
//! sandbox.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::RwLock;
use std::time::Instant;

use prometheus::CounterVec;
use prometheus::HistogramVec;
use prometheus::Opts;
use wasmtime::StoreLimits;

use crate::wasm::bindings::spire::filter::types as wit;

pub const CAP_LOG: &str = "host.log";
pub const CAP_METRICS: &str = "host.metrics";
pub const CAP_SHARED_DATA: &str = "host.shared-data";
pub const CAP_CLOCK: &str = "host.clock";

/// Resolved budgets for one (plugin, route) pair. Operator-controlled: the
/// manifest may not raise them.
#[derive(Debug, Clone)]
pub struct PluginLimits {
    pub timeout: std::time::Duration,
    pub fuel: u64,
    pub max_memory_bytes: usize,
    /// Consumed by the M2 body phases; declared here so the config surface and
    /// the runtime budget stay one type.
    #[allow(dead_code)]
    pub max_body_bytes: usize,
    pub max_output_bytes: usize,
}

impl PluginLimits {
    pub fn from_config(cfg: &crate::middleware::wasm_plugin_config::WasmPluginConfig) -> Self {
        Self {
            timeout: std::time::Duration::from_millis(cfg.timeout_ms as u64),
            fuel: cfg.fuel,
            max_memory_bytes: cfg.max_memory_bytes as usize,
            max_body_bytes: cfg.max_body_bytes as usize,
            max_output_bytes: cfg.max_output_bytes as usize,
        }
    }
}

/// Budget for `host.log` per exchange. A plugin that logs in a loop must not be
/// able to turn a request into a log flood (or a syscall storm).
pub const LOG_BUDGET: u32 = 64;

/// Per-instance state.
pub struct StoreData {
    pub plugin_id: String,
    pub granted: Arc<Vec<String>>,
    pub config: Arc<str>,
    pub limits: PluginLimits,
    /// Wall-clock deadline of the *current* callback. Read from the epoch
    /// callback, written before every call.
    pub deadline: Cell<Instant>,
    pub log_budget: Cell<u32>,
    pub shared: Arc<SharedData>,
    pub counters: Arc<PluginMetrics>,
    /// Handed to `Store::limiter`; the linear-memory ceiling.
    pub store_limits: StoreLimits,
    /// Deny-all WASI context: a `wasm32-wasip2` guest links against it, but it
    /// can reach no file, environment variable, argument or socket through it.
    pub wasi: wasmtime_wasi::WasiCtx,
    pub resources: wasmtime::component::ResourceTable,
}

impl StoreData {
    pub fn new(
        plugin_id: String,
        granted: Arc<Vec<String>>,
        config: Arc<str>,
        limits: PluginLimits,
        shared: Arc<SharedData>,
        counters: Arc<PluginMetrics>,
    ) -> Self {
        let store_limits = wasmtime::StoreLimitsBuilder::new()
            .memory_size(limits.max_memory_bytes)
            // A component needs a few tables/instances for its own module
            // graph; these are structural, not data-sized.
            .instances(16)
            .tables(64)
            .memories(4)
            .trap_on_grow_failure(true)
            .build();
        Self {
            plugin_id,
            granted,
            config,
            limits,
            deadline: Cell::new(Instant::now()),
            log_budget: Cell::new(LOG_BUDGET),
            shared,
            counters,
            store_limits,
            // `WasiCtx::builder()` defaults are already deny-all: stdin closed,
            // stdout/stderr discarded, no env, no args, no preopens, sockets
            // denied. Kept explicit so a future default change is a deliberate
            // decision rather than a silent widening of the sandbox.
            wasi: wasmtime_wasi::WasiCtx::builder().build(),
            resources: wasmtime::component::ResourceTable::new(),
        }
    }

    fn require(&self, capability: &str) -> Result<(), wit::Error> {
        if self.granted.iter().any(|g| g == capability) {
            Ok(())
        } else {
            Err(wit::Error::Denied(capability.to_string()))
        }
    }
}

impl wasmtime_wasi::WasiView for StoreData {
    fn ctx(&mut self) -> wasmtime_wasi::WasiCtxView<'_> {
        wasmtime_wasi::WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.resources,
        }
    }
}

impl bindings::spire::filter::types::Host for StoreData {}

impl bindings::spire::filter::host::Host for StoreData {
    fn log(&mut self, level: wit::LogLevel, message: String) {
        if self.require(CAP_LOG).is_err() {
            return;
        }
        let budget = self.log_budget.get();
        if budget == 0 {
            return;
        }
        self.log_budget.set(budget - 1);
        let plugin = self.plugin_id.as_str();
        // Truncation happens on a char boundary: a plugin can send arbitrary
        // bytes as a `string`, and a log line is not worth a panic.
        let message = truncate(&message, 2048);
        match level {
            wit::LogLevel::Trace => trace!(plugin = plugin, "{message}"),
            wit::LogLevel::Debug => debug!(plugin = plugin, "{message}"),
            wit::LogLevel::Info => info!(plugin = plugin, "{message}"),
            wit::LogLevel::Warn => warn!(plugin = plugin, "{message}"),
            wit::LogLevel::Error => error!(plugin = plugin, "{message}"),
        }
        if budget == 1 {
            warn!(plugin = plugin, "log budget exhausted for this exchange");
        }
    }

    fn config_json(&mut self) -> String {
        self.config.to_string()
    }

    fn incr_counter(
        &mut self,
        name: String,
        labels: Vec<(String, String)>,
        delta: u64,
    ) -> Result<(), wit::Error> {
        self.require(CAP_METRICS)?;
        let metric = self.counters.counter(&self.plugin_id, &name, &labels)?;
        
        let values = PluginMetrics::label_values(&labels);
        metric.with_label_values(&values).inc_by(delta as f64);
        Ok(())
    }

    fn observe_histogram(
        &mut self,
        name: String,
        labels: Vec<(String, String)>,
        value_f64: f64,
    ) -> Result<(), wit::Error> {
        self.require(CAP_METRICS)?;
        if !value_f64.is_finite() {
            return Err(wit::Error::Invalid(format!(
                "histogram `{name}`: value is not finite"
            )));
        }
        let metric = self
            .counters
            .histogram(&self.plugin_id, &name, &labels)?;
        let values = PluginMetrics::label_values(&labels);
        metric.with_label_values(&values).observe(value_f64);
        Ok(())
    }

    fn get_shared_data(&mut self, namespace: String, key: String) -> Option<Vec<u8>> {
        if self.require(CAP_SHARED_DATA).is_err() {
            return None;
        }
        self.shared.get(&namespace, &key)
    }

    fn set_shared_data(
        &mut self,
        namespace: String,
        key: String,
        value: Vec<u8>,
    ) -> Result<(), wit::Error> {
        self.require(CAP_SHARED_DATA)?;
        self.shared.set(&namespace, &key, value).map_err(wit::Error::Invalid)
    }

    fn now_millis(&mut self) -> u64 {
        if self.require(CAP_CLOCK).is_err() {
            return 0;
        }
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

use super::bindings;

fn truncate(message: &str, max_bytes: usize) -> &str {
    if message.len() <= max_bytes {
        return message;
    }
    let mut end = max_bytes;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    &message[..end]
}

// ---------------------------------------------------------------------------
// Shared state: the one place a plugin can keep data that outlives a request
// and is visible from every worker thread.
// ---------------------------------------------------------------------------

const SHARED_MAX_ENTRIES: usize = 1024;
const SHARED_MAX_BYTES: usize = 4 * 1024 * 1024;
const SHARED_MAX_VALUE: usize = 256 * 1024;

/// Process-wide, per-plugin key/value store.
///
/// Bounded on purpose: an unbounded map inside the gateway is an OOM waiting
/// for a plugin bug. Every write that would exceed a cap fails with
/// `error::invalid`, which the guest can observe and handle.
#[derive(Debug, Default)]
pub struct SharedData {
    inner: RwLock<SharedStore>,
}

#[derive(Debug, Default)]
struct SharedStore {
    entries: HashMap<(String, String), Vec<u8>>,
    bytes: usize,
}

impl SharedData {
    pub fn get(&self, namespace: &str, key: &str) -> Option<Vec<u8>> {
        let Ok(store) = self.inner.read() else {
            return None;
        };
        store
            .entries
            .get(&(namespace.to_string(), key.to_string()))
            .cloned()
    }

    pub fn set(&self, namespace: &str, key: &str, value: Vec<u8>) -> Result<(), String> {
        if value.len() > SHARED_MAX_VALUE {
            return Err(format!(
                "value is {} bytes, over the {SHARED_MAX_VALUE} byte cap",
                value.len()
            ));
        }
        let mut store = self
            .inner
            .write()
            .map_err(|_| "shared data lock poisoned".to_string())?;
        let id = (namespace.to_string(), key.to_string());
        let previous = store.entries.get(&id).map(|v| v.len()).unwrap_or(0);
        let new_total = store.bytes - previous + value.len();
        if new_total > SHARED_MAX_BYTES {
            return Err(format!(
                "shared data would reach {new_total} bytes, over the {SHARED_MAX_BYTES} byte cap"
            ));
        }
        if previous == 0 && store.entries.len() >= SHARED_MAX_ENTRIES {
            return Err(format!(
                "shared data already holds {SHARED_MAX_ENTRIES} keys"
            ));
        }
        store.bytes = new_total;
        store.entries.insert(id, value);
        Ok(())
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner.read().map(|s| s.entries.len()).unwrap_or(0)
    }
}

// ---------------------------------------------------------------------------
// Plugin-defined metrics
// ---------------------------------------------------------------------------

const MAX_PLUGIN_METRICS: usize = 64;
const MAX_PLUGIN_LABELS: usize = 10;
const MAX_LABEL_LEN: usize = 128;

enum MetricSlot {
    Counter(CounterVec, Vec<String>),
    Histogram(HistogramVec, Vec<String>),
}

/// Lazily-registered counters/histograms, namespaced under `spire_plugin_`.
///
/// Options are derived from the plugin's own arguments, which Prometheus
/// requires to be stable: the first call fixes the label set and any later call
/// with different label names is rejected as `invalid` instead of panicking the
/// registry.
#[derive(Default)]
pub struct PluginMetrics {
    inner: RwLock<HashMap<(String, String), MetricSlot>>,
}

impl PluginMetrics {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Prometheus requires every descriptor that shares a fully-qualified name
    /// to agree on its help string and label set, across the whole process. Two
    /// plugins using the same counter name would therefore collide, and a unit
    /// test in another module would be able to break a plugin. So the plugin id
    /// is part of the metric name, not only a label:
    /// `spire_plugin_<plugin>_<name>_total{plugin="…", …}`.
    fn metric_name(plugin: &str, name: &str) -> Result<String, wit::Error> {
        if name.is_empty() || name.len() > 100 {
            return Err(wit::Error::Invalid(
                "metric name must be 1..=100 characters".to_string(),
            ));
        }
        if !name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase())
            || !name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            return Err(wit::Error::Invalid(format!(
                "metric name `{name}` must match [a-z][a-z0-9_]*"
            )));
        }
        let sanitized: String = plugin
            .chars()
            .map(|c| {
                if c.is_ascii_lowercase() || c.is_ascii_digit() {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        Ok(format!("spire_plugin_{sanitized}_{name}"))
    }

    fn label_names(labels: &[(String, String)]) -> Result<Vec<String>, wit::Error> {
        if labels.len() > MAX_PLUGIN_LABELS {
            return Err(wit::Error::Invalid(format!(
                "at most {MAX_PLUGIN_LABELS} labels are allowed, got {}",
                labels.len()
            )));
        }
        let mut names = Vec::with_capacity(labels.len());
        for (name, _) in labels {
            if name.is_empty()
                || name.len() > 64
                || !name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            {
                return Err(wit::Error::Invalid(format!(
                    "label name `{name}` must match [a-z0-9_]{{1,64}}"
                )));
            }
            if name == "plugin" {
                return Err(wit::Error::Invalid(
                    "`plugin` is a reserved label, added by the host".to_string(),
                ));
            }
            names.push(name.clone());
        }
        Ok(names)
    }

    /// Label values for the *variable* labels. The plugin id is a constant
    /// label: Prometheus identifies a collector by its metric name, so two
    /// plugins (or two configurations) using the same counter name would
    /// otherwise collide in the registry instead of coexisting.
    fn label_values(labels: &[(String, String)]) -> Vec<String> {
        labels
            .iter()
            .map(|(_, value)| {
                let mut value = value.clone();
                if value.len() > MAX_LABEL_LEN {
                    value.truncate(MAX_LABEL_LEN);
                }
                value
            })
            .collect()
    }

    fn const_labels(plugin: &str) -> HashMap<String, String> {
        let mut map = HashMap::with_capacity(1);
        map.insert("plugin".to_string(), plugin.to_string());
        map
    }

    pub fn counter(
        &self,
        plugin: &str,
        name: &str,
        labels: &[(String, String)],
    ) -> Result<CounterVec, wit::Error> {
        let metric_name = Self::metric_name(plugin, name)?;
        let names = Self::label_names(labels)?;
        let key = (plugin.to_string(), name.to_string());
        {
            let map = self
                .inner
                .read()
                .map_err(|_| wit::Error::Failed("metric registry poisoned".to_string()))?;
            if let Some(slot) = map.get(&key) {
                return match slot {
                    MetricSlot::Counter(vec, registered) if registered == &names => Ok(vec.clone()),
                    MetricSlot::Counter(_, registered) => Err(wit::Error::Invalid(format!(
                        "counter `{name}` was registered with labels {registered:?}, not {names:?}"
                    ))),
                    MetricSlot::Histogram(..) => Err(wit::Error::Invalid(format!(
                        "`{name}` is already registered as a histogram"
                    ))),
                };
            }
        }
        let mut map = self
            .inner
            .write()
            .map_err(|_| wit::Error::Failed("metric registry poisoned".to_string()))?;
        if map.len() >= MAX_PLUGIN_METRICS {
            return Err(wit::Error::Invalid(format!(
                "plugin already defined {MAX_PLUGIN_METRICS} metrics"
            )));
        }
        let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let vec = CounterVec::new(
            Opts::new(
                format!("{metric_name}_total"),
                format!("Guest-defined counter `{name}` (plugin {plugin})."),
            )
            .const_labels(Self::const_labels(plugin)),
            &name_refs,
        )
        .map_err(|e| wit::Error::Invalid(format!("counter `{name}`: {e}")))?;
        // `CounterVec::new` only *builds* the vector; it has to be registered
        // for `prometheus::gather()` (and therefore `/metrics`) to see it.
        prometheus::register(Box::new(vec.clone())).map_err(|e| {
            wit::Error::Invalid(format!("counter `{name}` cannot be registered: {e}"))
        })?;
        map.insert(key, MetricSlot::Counter(vec.clone(), names));
        Ok(vec)
    }

    pub fn histogram(
        &self,
        plugin: &str,
        name: &str,
        labels: &[(String, String)],
    ) -> Result<HistogramVec, wit::Error> {
        let metric_name = Self::metric_name(plugin, name)?;
        let names = Self::label_names(labels)?;
        let key = (plugin.to_string(), name.to_string());
        {
            let map = self
                .inner
                .read()
                .map_err(|_| wit::Error::Failed("metric registry poisoned".to_string()))?;
            if let Some(slot) = map.get(&key) {
                return match slot {
                    MetricSlot::Histogram(vec, registered) if registered == &names => {
                        Ok(vec.clone())
                    }
                    MetricSlot::Histogram(_, registered) => Err(wit::Error::Invalid(format!(
                        "histogram `{name}` was registered with labels {registered:?}, not {names:?}"
                    ))),
                    MetricSlot::Counter(..) => Err(wit::Error::Invalid(format!(
                        "`{name}` is already registered as a counter"
                    ))),
                };
            }
        }
        let mut map = self
            .inner
            .write()
            .map_err(|_| wit::Error::Failed("metric registry poisoned".to_string()))?;
        if map.len() >= MAX_PLUGIN_METRICS {
            return Err(wit::Error::Invalid(format!(
                "plugin already defined {MAX_PLUGIN_METRICS} metrics"
            )));
        }
        let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let vec = HistogramVec::new(
            prometheus::HistogramOpts::new(
                metric_name,
                format!("Guest-defined histogram `{name}` (plugin {plugin})."),
            )
            .const_labels(Self::const_labels(plugin)),
            &name_refs,
        )
        .map_err(|e| wit::Error::Invalid(format!("histogram `{name}`: {e}")))?;
        prometheus::register(Box::new(vec.clone())).map_err(|e| {
            wit::Error::Invalid(format!("histogram `{name}` cannot be registered: {e}"))
        })?;
        map.insert(key, MetricSlot::Histogram(vec.clone(), names));
        Ok(vec)
    }
}

/// Process-wide shared state. One map, shared by every plugin instance on every
/// thread: the reason it exists is that per-instance state cannot implement a
/// gateway-wide rate limiter or cache.
pub fn shared_data() -> &'static Arc<SharedData> {
    static SHARED: OnceLock<Arc<SharedData>> = OnceLock::new();
    SHARED.get_or_init(|| Arc::new(SharedData::default()))
}

pub fn plugin_metrics() -> &'static Arc<PluginMetrics> {
    static METRICS: OnceLock<Arc<PluginMetrics>> = OnceLock::new();
    METRICS.get_or_init(PluginMetrics::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_data_is_bounded() {
        let shared = SharedData::default();
        shared.set("ns", "k", vec![1, 2, 3]).unwrap();
        assert_eq!(shared.get("ns", "k").unwrap(), vec![1, 2, 3]);
        assert_eq!(shared.len(), 1);
        let too_big = vec![0u8; SHARED_MAX_VALUE + 1];
        assert!(shared.set("ns", "big", too_big).is_err());
    }

    #[test]
    fn metric_names_and_labels_are_validated() {
        let metrics = PluginMetrics::default();
        assert!(metrics.counter("p", "Bad-Name", &[]).is_err());
        assert!(metrics.counter("p", "requests", &[]).is_ok());
        // Second registration with the same label set reuses the vector.
        assert!(metrics.counter("p", "requests", &[]).is_ok());
        let labels = vec![("code".to_string(), "200".to_string())];
        assert!(metrics.counter("p", "requests", &labels).is_err());
    }

    #[test]
    fn guest_counter_reaches_the_default_registry() {
        let metrics = PluginMetrics::default();
        let labels = vec![("code".to_string(), "200".to_string())];
        let vec = metrics
            .counter("io.example.probe", "probe_requests", &labels)
            .expect("counter should register");
        let values = PluginMetrics::label_values(&labels);
        vec.with_label_values(&values).inc();
        assert!(
            prometheus::gather()
                .into_iter()
                .any(|family| family.name() == "spire_plugin_io_example_probe_probe_requests_total"),
            "a guest-defined counter must show up in gather()"
        );
    }

    #[test]
    fn capability_gating_reports_denied() {        let data = StoreData::new(
            "io.example.x".to_string(),
            Arc::new(vec![CAP_LOG.to_string()]),
            Arc::from(""),
            PluginLimits::from_config(
                &crate::middleware::wasm_plugin_config::WasmPluginConfig {
                    plugin: "io.example.x".to_string(),
                    config: None,
                    fail: Default::default(),
                    body: Default::default(),
                    timeout_ms: 5,
                    fuel: 1_000_000,
                    max_memory_bytes: 1024 * 1024,
                    max_body_bytes: 1024,
                    max_output_bytes: 1024,
                },
            ),
            shared_data().clone(),
            plugin_metrics().clone(),
        );
        assert!(data.require(CAP_LOG).is_ok());
        assert!(matches!(
            data.require(CAP_METRICS),
            Err(wit::Error::Denied(_))
        ));
    }
}
