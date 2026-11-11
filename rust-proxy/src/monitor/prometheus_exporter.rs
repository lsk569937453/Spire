use prometheus::{
    CounterVec, Gauge, HistogramVec, IntGaugeVec, labels, opts, register_counter_vec,
    register_gauge, register_histogram_vec, register_int_gauge_vec,
};
use std::sync::OnceLock;

pub mod metrics {
    use delay_timer::anyhow;

    use super::*;

    // --------------------------
    // All metrics defined here
    // --------------------------

    pub static HTTP_REQUESTS_TOTAL: OnceLock<CounterVec> = OnceLock::new();
    pub static HTTP_REQUEST_DURATION_SECONDS: OnceLock<HistogramVec> = OnceLock::new();

    pub static HTTP_COUNTER: OnceLock<CounterVec> = OnceLock::new();
    pub static HTTP_BODY_GAUGE: OnceLock<Gauge> = OnceLock::new();
    pub static HTTP_REQ_HISTOGRAM: OnceLock<HistogramVec> = OnceLock::new();

    /// Upstream requests that hit the route timeout (headers phase) or the
    /// body idle timeout. The slow-downstream signal.
    pub static SPIRE_UPSTREAM_TIMEOUTS_TOTAL: OnceLock<CounterVec> = OnceLock::new();
    /// Requests rejected before touching the upstream because the endpoint
    /// concurrency limit was exhausted. Backpressure doing its job.
    pub static SPIRE_UPSTREAM_CONCURRENCY_REJECTED_TOTAL: OnceLock<CounterVec> = OnceLock::new();
    /// Upstream latency split by phase (`ttfb` = until response headers,
    /// `body` = headers until last body frame).
    pub static SPIRE_UPSTREAM_DURATION_SECONDS: OnceLock<HistogramVec> = OnceLock::new();
    /// In-flight upstream requests per endpoint; a permit is held from before
    /// the upstream call until the response body has fully streamed.
    pub static SPIRE_UPSTREAM_IN_FLIGHT: OnceLock<IntGaugeVec> = OnceLock::new();
    /// Configured concurrency limit behind each endpoint.
    pub static SPIRE_UPSTREAM_CONCURRENCY_LIMIT: OnceLock<IntGaugeVec> = OnceLock::new();
    /// Circuit breaker state per route: 0 closed, 1 open, 2 half-open.
    pub static SPIRE_CIRCUIT_BREAKER_STATE: OnceLock<IntGaugeVec> = OnceLock::new();
    /// Memory limit the concurrency budget was derived from (0 when no
    /// limit could be detected).
    pub static SPIRE_MEMORY_LIMIT_BYTES: OnceLock<Gauge> = OnceLock::new();
    /// Total permits in the process-wide memory guard (the OOM backstop).
    pub static SPIRE_UPSTREAM_GLOBAL_PERMITS: OnceLock<Gauge> = OnceLock::new();

    // --------------------------
    // Unified initialization
    // --------------------------

    /// Registers the metric vectors.
    ///
    /// Labels are deliberately low cardinality: `route` is the matched route
    /// id and `endpoint` the configured upstream authority (`scheme://host:
    /// port`), never the raw request path. Using the path (with its query
    /// string) as a label creates one time series per distinct URL, which
    /// grows the process memory and the scrape payload without bound.
    pub fn init() -> Result<(), anyhow::Error> {
        match register_counter_vec!(
            "http_requests_total",
            "Total number of HTTP requests.",
            &["mapping_key", "route", "method", "status"]
        ) {
            Ok(metric) => {
                HTTP_REQUESTS_TOTAL.get_or_init(|| metric);
            }
            Err(e) => warn!("Failed to register http_requests_total: {e}"),
        }

        match register_histogram_vec!(
            "http_request_duration_seconds",
            "The HTTP request latencies in seconds.",
            &["mapping_key", "route", "method"]
        ) {
            Ok(metric) => {
                HTTP_REQUEST_DURATION_SECONDS.get_or_init(|| metric);
            }
            Err(e) => warn!("Failed to register http_request_duration_seconds: {e}"),
        }

        match register_counter_vec!(
            opts!("spire_http_requests_total", "Number of HTTP requests made."),
            &["port", "route", "status_code"]
        ) {
            Ok(metric) => {
                HTTP_COUNTER.get_or_init(|| metric);
            }
            Err(e) => warn!("Failed to register spire_http_requests_total: {e}"),
        }

        match register_gauge!(opts!(
            "spire_http_response_size_bytes",
            "The HTTP response sizes in bytes.",
            labels! {"handler" => "all",}
        )) {
            Ok(metric) => {
                HTTP_BODY_GAUGE.get_or_init(|| metric);
            }
            Err(e) => warn!("Failed to register spire_http_response_size_bytes: {e}"),
        }

        match register_histogram_vec!(
            "spire_http_request_duration_seconds",
            "The HTTP request latencies in seconds.",
            &["port", "route"]
        ) {
            Ok(metric) => {
                HTTP_REQ_HISTOGRAM.get_or_init(|| metric);
            }
            Err(e) => warn!("Failed to register spire_http_request_duration_seconds: {e}"),
        }

        match register_counter_vec!(
            opts!(
                "spire_upstream_timeouts_total",
                "Upstream requests that hit the route timeout (headers phase or body idle timeout)."
            ),
            &["mapping_key", "route"]
        ) {
            Ok(metric) => {
                SPIRE_UPSTREAM_TIMEOUTS_TOTAL.get_or_init(|| metric);
            }
            Err(e) => warn!("Failed to register spire_upstream_timeouts_total: {e}"),
        }

        match register_counter_vec!(
            opts!(
                "spire_upstream_concurrency_rejected_total",
                "Requests rejected because the endpoint concurrency limit was exhausted."
            ),
            &["mapping_key", "route", "endpoint"]
        ) {
            Ok(metric) => {
                SPIRE_UPSTREAM_CONCURRENCY_REJECTED_TOTAL.get_or_init(|| metric);
            }
            Err(e) => warn!("Failed to register spire_upstream_concurrency_rejected_total: {e}"),
        }

        match register_histogram_vec!(
            "spire_upstream_duration_seconds",
            "Upstream request latency in seconds, split by phase (ttfb, body).",
            &["mapping_key", "route", "endpoint", "phase"]
        ) {
            Ok(metric) => {
                SPIRE_UPSTREAM_DURATION_SECONDS.get_or_init(|| metric);
            }
            Err(e) => warn!("Failed to register spire_upstream_duration_seconds: {e}"),
        }

        match register_int_gauge_vec!(
            opts!(
                "spire_upstream_in_flight",
                "In-flight upstream requests per endpoint (permit held until the response body has fully streamed)."
            ),
            &["mapping_key", "route", "endpoint"]
        ) {
            Ok(metric) => {
                SPIRE_UPSTREAM_IN_FLIGHT.get_or_init(|| metric);
            }
            Err(e) => warn!("Failed to register spire_upstream_in_flight: {e}"),
        }

        match register_int_gauge_vec!(
            opts!(
                "spire_upstream_concurrency_limit",
                "Configured concurrency limit per endpoint."
            ),
            &["mapping_key", "route", "endpoint"]
        ) {
            Ok(metric) => {
                SPIRE_UPSTREAM_CONCURRENCY_LIMIT.get_or_init(|| metric);
            }
            Err(e) => warn!("Failed to register spire_upstream_concurrency_limit: {e}"),
        }

        match register_int_gauge_vec!(
            opts!(
                "spire_circuit_breaker_state",
                "Circuit breaker state per route: 0 closed, 1 open, 2 half-open."
            ),
            &["route"]
        ) {
            Ok(metric) => {
                SPIRE_CIRCUIT_BREAKER_STATE.get_or_init(|| metric);
            }
            Err(e) => warn!("Failed to register spire_circuit_breaker_state: {e}"),
        }

        match register_gauge!(opts!(
            "spire_memory_limit_bytes",
            "Memory limit the upstream concurrency budget was derived from (0 = not detected)."
        )) {
            Ok(metric) => {
                SPIRE_MEMORY_LIMIT_BYTES.get_or_init(|| metric);
            }
            Err(e) => warn!("Failed to register spire_memory_limit_bytes: {e}"),
        }

        match register_gauge!(opts!(
            "spire_upstream_global_permits",
            "Total permits in the process-wide upstream memory guard (the OOM backstop)."
        )) {
            Ok(metric) => {
                SPIRE_UPSTREAM_GLOBAL_PERMITS.get_or_init(|| metric);
            }
            Err(e) => warn!("Failed to register spire_upstream_global_permits: {e}"),
        }
        Ok(())
    }

    // --------------------------
    // Best-effort recording helpers
    // --------------------------
    // Metrics must never change the response a client receives: every helper
    // silently does nothing when its collector failed to register.

    pub fn record_upstream_timeout(mapping_key: &str, route: &str) {
        if let Some(counter) = SPIRE_UPSTREAM_TIMEOUTS_TOTAL.get() {
            counter
                .with_label_values(&[mapping_key, route])
                .inc();
        }
    }

    pub fn record_concurrency_rejected(mapping_key: &str, route: &str, endpoint: &str) {
        if let Some(counter) = SPIRE_UPSTREAM_CONCURRENCY_REJECTED_TOTAL.get() {
            counter
                .with_label_values(&[mapping_key, route, endpoint])
                .inc();
        }
    }

    pub fn observe_upstream_duration(
        mapping_key: &str,
        route: &str,
        endpoint: &str,
        phase: &str,
        seconds: f64,
    ) {
        if let Some(histogram) = SPIRE_UPSTREAM_DURATION_SECONDS.get() {
            histogram
                .with_label_values(&[mapping_key, route, endpoint, phase])
                .observe(seconds);
        }
    }

    pub fn upstream_in_flight_delta(mapping_key: &str, route: &str, endpoint: &str, delta: i64) {
        if let Some(gauge) = SPIRE_UPSTREAM_IN_FLIGHT.get() {
            gauge.with_label_values(&[mapping_key, route, endpoint]).add(delta);
        }
    }

    pub fn set_upstream_concurrency_limit(mapping_key: &str, route: &str, endpoint: &str, limit: i64) {
        if let Some(gauge) = SPIRE_UPSTREAM_CONCURRENCY_LIMIT.get() {
            gauge
                .with_label_values(&[mapping_key, route, endpoint])
                .set(limit);
        }
    }

    pub fn set_circuit_breaker_state(route: &str, state: i64) {
        if let Some(gauge) = SPIRE_CIRCUIT_BREAKER_STATE.get() {
            gauge.with_label_values(&[route]).set(state);
        }
    }

    pub fn set_memory_limit_bytes(bytes: f64) {
        if let Some(gauge) = SPIRE_MEMORY_LIMIT_BYTES.get() {
            gauge.set(bytes);
        }
    }

    pub fn set_upstream_global_permits(permits: f64) {
        if let Some(gauge) = SPIRE_UPSTREAM_GLOBAL_PERMITS.get() {
            gauge.set(permits);
        }
    }
}
