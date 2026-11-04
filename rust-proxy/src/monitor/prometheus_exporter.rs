use prometheus::{
    CounterVec, Gauge, HistogramVec, labels, opts, register_counter_vec, register_gauge,
    register_histogram_vec,
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

    // --------------------------
    // Unified initialization
    // --------------------------

    /// Registers the metric vectors.
    ///
    /// Labels are deliberately low cardinality: `route` is the matched route id,
    /// never the raw request path. Using the path (with its query string) as a
    /// label creates one time series per distinct URL, which grows the process
    /// memory and the scrape payload without bound.
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
        Ok(())
    }
}
