//! Upstream forwarding for the HTTP/1 proxy.
//!
//! `http_proxy::proxy` decides *what* to do with a matched request; this module
//! owns the mechanics of talking to the upstream (concurrency permits, time
//! budget, body wrapping) and the gateway responses Spire sends on its own.

use crate::constants::common_constants::MAX_TRANSCODE_BODY_BYTES;
use crate::monitor::prometheus_exporter::metrics;
use crate::proxy::concurrency;
use crate::proxy::http1::app_clients::AppClients;
use crate::proxy::http1::upstream_body::PermitBody;
use crate::proxy::http1::upstream_body::TimedBody;
use crate::proxy::http1::upstream_body::UpstreamLabels;
use crate::vojo::app_error::AppError;
use crate::vojo::concurrency::ConcurrencyConfig;
use bytes::Bytes;
use http::{HeaderName, HeaderValue};
use http_body_util::BodyExt;
use http_body_util::Full;
use http_body_util::combinators::BoxBody;
use hyper::StatusCode;
use hyper::header;
use hyper::{Request, Response};
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use tokio::time::timeout;

/// 504 Gateway Timeout for an upstream that exceeded its time budget.
///
/// Returned as a *successful* result (not `Err`) so it flows through
/// `handle_before_response`: the circuit breaker's `record_outcome` must see
/// timeouts as failures — a slow-to-death downstream is the one failure mode
/// the breaker used to be blind to.
pub(super) fn gateway_timeout_response() -> Result<Response<BoxBody<Bytes, AppError>>, AppError> {
    let response = Response::builder()
        .status(StatusCode::GATEWAY_TIMEOUT)
        .header(header::RETRY_AFTER, "1")
        .header(header::CONTENT_TYPE, "application/json")
        .body(
            Full::new(Bytes::from_static(b"{\"error\":\"upstream timeout\"}"))
                .map_err(|e| match e {})
                .boxed(),
        )?;
    Ok(response)
}

/// 503 Service Unavailable for our own backpressure (concurrency limit
/// exhausted). Returned on the early path that bypasses `record_outcome`:
/// the breaker judges the downstream's health, never our own load shedding.
/// The endpoint is deliberately kept out of the body; it is logged instead.
pub(super) fn service_unavailable_response() -> Result<Response<BoxBody<Bytes, AppError>>, AppError>
{
    let response = Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .header(header::RETRY_AFTER, "1")
        .header(header::CONTENT_TYPE, "application/json")
        .body(
            Full::new(Bytes::from_static(
                b"{\"error\":\"upstream concurrency limit\"}",
            ))
            .map_err(|e| match e {})
            .boxed(),
        )?;
    Ok(response)
}

/// The per-request facts the forwarding helpers need from the matched route.
///
/// Bundled so the helpers never take a partially moved `HandlingResult` (its
/// `router_destination` is matched by value in `proxy`).
pub(super) struct UpstreamCall<'a> {
    pub(super) endpoint: &'a str,
    pub(super) timeout_ms: u64,
    pub(super) concurrency: &'a ConcurrencyConfig,
    pub(super) mapping_key: &'a str,
    pub(super) route_label: &'a str,
}

/// Forwards an HTTP request to the matched upstream, holding its concurrency
/// permit until the response body has fully streamed.
pub(super) async fn forward_http_upstream<B>(
    client: &AppClients,
    req: Request<B>,
    call: UpstreamCall<'_>,
    request_path: &str,
) -> Result<Response<BoxBody<Bytes, AppError>>, AppError>
where
    B: http_body::Body<Data = Bytes> + Send + Sync + std::fmt::Debug + Unpin + 'static,
    AppError: From<B::Error>,
{
    let timeout = call.timeout_ms;

    // Backpressure: a concurrency permit must be held before the
    // upstream is touched, otherwise in-flight requests are unbounded
    // (the async equivalent of a full worker thread pool, except the
    // process dies of OOM instead of queueing).
    let labels = UpstreamLabels {
        mapping_key: Arc::from(call.mapping_key),
        route: Arc::from(call.route_label),
        endpoint: concurrency::endpoint_key(call.endpoint),
    };
    let permit = match concurrency::acquire(
        call.endpoint,
        call.concurrency,
        &labels.mapping_key,
        &labels.route,
    )
    .await
    {
        Ok(permit) => permit,
        Err(rejection) => {
            warn!(
                "[ConcurrencyLimit] {rejection} (route {})",
                labels.route.as_ref()
            );
            // Early return: our own load shedding is not a downstream
            // failure and must not feed the circuit breaker.
            return service_unavailable_response();
        }
    };
    metrics::upstream_in_flight_delta(&labels.mapping_key, &labels.route, &labels.endpoint, 1);

    let req = req.map(|b| b.map_err(AppError::from).boxed());
    // Decide on the *scheme*, not by looking for the substring "https"
    // somewhere in the path (a query string like `?next=https://...`
    // used to flip the transport).
    let request_future = if request_path.starts_with("https://") {
        client.http.request_https(req, timeout)
    } else {
        client.http.request_http(req, timeout)
    };
    let started = Instant::now();
    // `Ok(None)` = timed out: recorded below and answered with 504
    // *as this match arm's value*, so it keeps flowing through
    // handle_before_response — that is the whole fix. Returning
    // early here is exactly how the breaker used to go blind.
    // The permit lives in an Option because ownership splits across
    // the mutually exclusive branches below (taken by the timeout /
    // error paths, handed to PermitBody on success).
    let mut permit = Some(permit);
    let outcome: Result<Option<_>, _> = match request_future.await {
        Ok(response) => response.map(Some).map_err(|e| AppError(e.to_string())),
        Err(_elapsed) => {
            metrics::observe_upstream_duration(
                &labels.mapping_key,
                &labels.route,
                &labels.endpoint,
                "ttfb",
                started.elapsed().as_secs_f64(),
            );
            metrics::record_upstream_timeout(&labels.mapping_key, &labels.route);
            error!(
                "Upstream request timed out after {timeout} ms: {request_path} (route {})",
                labels.route
            );
            // No PermitBody will exist for this request: undo the
            // in-flight count and let the permit go here.
            drop(permit.take());
            metrics::upstream_in_flight_delta(
                &labels.mapping_key,
                &labels.route,
                &labels.endpoint,
                -1,
            );
            Ok(None)
        }
    };
    match outcome {
        Ok(Some(response)) => {
            let ttfb = started.elapsed();
            metrics::observe_upstream_duration(
                &labels.mapping_key,
                &labels.route,
                &labels.endpoint,
                "ttfb",
                ttfb.as_secs_f64(),
            );
            let (mut parts, body) = response.into_parts();
            // Debug aid: how much of the total latency was the upstream.
            parts.headers.insert(
                HeaderName::from_static("x-upstream-latency"),
                HeaderValue::from(ttfb.as_millis() as u64),
            );
            // The permit is only released when the body has fully
            // streamed (PermitBody::finish / drop); the idle watchdog
            // uses the same budget as the headers phase so a
            // slow-drip body cannot hold the request forever.
            let timed = TimedBody::new(
                body.boxed().map_err(|e| AppError(e.to_string())).boxed(),
                Duration::from_millis(timeout),
                labels.clone(),
            );
            let permit = permit.take().ok_or_else(|| {
                AppError("concurrency permit disappeared before the body was built".to_string())
            })?;
            let permit_body = PermitBody::new(timed.boxed(), permit, labels, Instant::now());
            Ok(Response::from_parts(parts, permit_body.boxed()))
        }
        Ok(None) => gateway_timeout_response(),
        Err(e) => {
            // Transport error (connect refused, reset, TLS, ...):
            // no body to own the permit, release it here. Flows on as
            // Err so the breaker records a failure, as before.
            drop(permit.take());
            metrics::upstream_in_flight_delta(
                &labels.mapping_key,
                &labels.route,
                &labels.endpoint,
                -1,
            );
            metrics::observe_upstream_duration(
                &labels.mapping_key,
                &labels.route,
                &labels.endpoint,
                "ttfb",
                started.elapsed().as_secs_f64(),
            );
            Err(e)
        }
    }
}

/// Forwards a gRPC request to the matched upstream and renders the reply as
/// JSON, holding the weighted concurrency permit across the call.
pub(super) async fn forward_grpc_upstream<B>(
    client: &AppClients,
    req: Request<B>,
    call: UpstreamCall<'_>,
    request_path: &str,
) -> Result<Response<BoxBody<Bytes, AppError>>, AppError>
where
    B: http_body::Body<Data = Bytes> + Send + Sync + std::fmt::Debug + Unpin + 'static,
    AppError: From<B::Error>,
{
    debug!("The request is grpc!,{request_path}");
    let grpc_client = client
        .grpc
        .as_ref()
        .ok_or(AppError(
            "No gRPC client configured for this listener".to_string(),
        ))?
        .get_client(call.endpoint)
        .await?;

    // Buffered body, so it must be bounded: an unbounded collect() is a
    // trivial memory exhaustion vector.
    let (_parts, mut body) = req.into_parts();
    let mut buffered = bytes::BytesMut::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(AppError::from)?;
        let Some(data) = frame.data_ref() else {
            continue;
        };
        if buffered.len() + data.len() > MAX_TRANSCODE_BODY_BYTES {
            return Err(AppError(format!(
                "Request body exceeds the {MAX_TRANSCODE_BODY_BYTES} byte limit"
            )));
        }
        buffered.extend_from_slice(data);
    }
    let body_bytes = buffered.freeze();
    let buffered_len = body_bytes.len();
    let parts: Vec<&str> = request_path.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() < 2 {
        return Err(AppError(format!(
            "Invalid gRPC target path, expected '/<service>/<method>': {request_path}"
        )));
    }
    let service_name = parts[0].to_string();
    let method_name = parts[1].to_string();

    // Same backpressure and time budget as the HTTP path. The
    // request body is fully buffered by now, so the global memory
    // guard can be charged by its actual size (×2: a roughly
    // similar-sized response + JSON expansion is expected).
    let global_weight = ((buffered_len * 2) / concurrency::PER_REQUEST_ESTIMATE_BYTES).max(1);
    let labels = UpstreamLabels {
        mapping_key: Arc::from(call.mapping_key),
        route: Arc::from(call.route_label),
        endpoint: concurrency::endpoint_key(call.endpoint),
    };
    let permit = match concurrency::acquire_weighted(
        call.endpoint,
        call.concurrency,
        global_weight,
        &labels.mapping_key,
        &labels.route,
    )
    .await
    {
        Ok(permit) => permit,
        Err(rejection) => {
            warn!(
                "[ConcurrencyLimit] {rejection} (route {})",
                labels.route.as_ref()
            );
            return service_unavailable_response();
        }
    };
    metrics::upstream_in_flight_delta(&labels.mapping_key, &labels.route, &labels.endpoint, 1);
    let started = Instant::now();
    // Ok(None) = timed out: answered with a 504 *as this match arm's
    // value* so it flows through handle_before_response and the
    // breaker records the failure (returning early would reintroduce
    // the blindness this fixes). Either way the upstream call is over
    // by then, so the permit is released and the gauge undone here —
    // the local JSON body below streams from memory, not upstream.
    let grpc_outcome: Result<Option<tonic::Response<prost_reflect::DynamicMessage>>, AppError> = {
        let response_future = timeout(
            Duration::from_millis(call.timeout_ms),
            grpc_client.do_request(service_name, method_name, body_bytes),
        )
        .await;
        metrics::observe_upstream_duration(
            &labels.mapping_key,
            &labels.route,
            &labels.endpoint,
            "ttfb",
            started.elapsed().as_secs_f64(),
        );
        drop(permit);
        metrics::upstream_in_flight_delta(&labels.mapping_key, &labels.route, &labels.endpoint, -1);
        match response_future {
            Ok(result) => result.map(Some),
            Err(_elapsed) => {
                metrics::record_upstream_timeout(&labels.mapping_key, &labels.route);
                error!(
                    "gRPC upstream request timed out after {} ms: {} (route {})",
                    call.timeout_ms, request_path, labels.route
                );
                Ok(None)
            }
        }
    };
    match grpc_outcome {
        Ok(None) => gateway_timeout_response(),
        Ok(Some(grpc_response)) => {
            let dynamic_message = grpc_response.into_inner();
            let response_json_string = serde_json::to_string(&dynamic_message)?;
            let response = Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/json")
                .body(
                    Full::new(Bytes::from(response_json_string))
                        .map_err(|e| match e {})
                        .boxed(),
                )?;
            Ok(response)
        }
        Err(e) => Err(e),
    }
}
