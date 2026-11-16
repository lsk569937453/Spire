//! Request, response and log phase plumbing shared by the HTTP/1 proxy paths.
//!
//! These helpers are called from `http_proxy::proxy` (and the WebSocket upgrade
//! branch it shares with plain HTTP), so they live next to it instead of inside
//! the dispatcher.

use crate::middleware::middlewares::MiddleWares;
use crate::middleware::middlewares::Middleware;
use crate::middleware::middlewares::{Denial, ExchangeSummary, RequestPhase, ResponsePhase};
use crate::proxy::proxy_trait::ChainTrait;
use crate::vojo::app_error::AppError;
use bytes::Bytes;
use http_body_util::BodyExt;
use http_body_util::Full;
use http_body_util::combinators::BoxBody;
use hyper::{Request, Response};
use std::net::SocketAddr;
use std::time::Duration;

/// Renders a denial — from routing (`allow_deny_list`, `authentication`, …) or
/// from a plugin's local reply — as the response the client receives.
pub(super) fn denial_response(
    denial: Denial,
) -> Result<Response<BoxBody<Bytes, AppError>>, AppError> {
    let mut response = Response::builder().status(denial.status).body(
        Full::new(Bytes::from(denial.body))
            .map_err(|e| match e {})
            .boxed(),
    )?;
    response.headers_mut().extend(denial.headers);
    Ok(response)
}

/// Per-exchange facts the plugin hooks need. Cheap to copy around.
#[derive(Clone, Copy)]
pub(super) struct PhaseInfo<'a> {
    pub(super) client_addr: SocketAddr,
    pub(super) route_id: &'a str,
    pub(super) request_id: &'a str,
    pub(super) listener_port: i32,
}

/// True when the route configured at least one WASM plugin. Everything the
/// plugin path costs (request-line copies, phase structs) is behind this.
pub(super) fn has_wasm(middlewares: &[MiddleWares]) -> bool {
    middlewares
        .iter()
        .any(|m| matches!(m, MiddleWares::Wasm(_)))
}

/// Runs the request-phase middlewares: first the built-in header rewriting
/// (`forward_headers`, `request_headers`), then the WASM plugins in configured
/// order.
///
/// Shared by the regular HTTP and the WebSocket upgrade paths so both forward
/// the same client information and both honour a plugin's local reply.
pub(super) async fn apply_request_middlewares<B>(
    chain_trait: &impl ChainTrait,
    middlewares: Option<Vec<MiddleWares>>,
    info: PhaseInfo<'_>,
    req: &mut Request<B>,
) -> Result<Option<Denial>, AppError>
where
    B: http_body::Body<Data = Bytes> + Send + Sync + std::fmt::Debug + Unpin + 'static,
    AppError: From<B::Error>,
{
    let Some(mut middlewares) = middlewares else {
        return Ok(None);
    };
    if middlewares.is_empty() {
        return Ok(None);
    }
    chain_trait
        .handle_before_request(&mut middlewares, info.client_addr, req.headers_mut())
        .await?;
    if !has_wasm(&middlewares) {
        return Ok(None);
    }

    let method = req.method().as_str().to_string();
    let uri = req.uri().clone();
    let scheme = uri.scheme_str().unwrap_or("http").to_string();
    let authority = uri
        .authority()
        .map(|a| a.as_str())
        .unwrap_or_default()
        .to_string();
    let path = uri.path().to_string();
    let query = uri.query().unwrap_or_default().to_string();
    let mut phase = RequestPhase {
        method,
        scheme,
        authority,
        path,
        query,
        headers: req.headers_mut(),
        client_addr: info.client_addr,
        route_id: info.route_id,
        listener_port: info.listener_port,
        request_id: info.request_id,
    };
    for middleware in middlewares.iter() {
        if let Some(denial) = middleware.on_request_phase(&mut phase)? {
            return Ok(Some(denial));
        }
    }
    Ok(None)
}

/// Runs the WASM response hooks before Spire's own response middlewares
/// (`cors`, `rewrite_headers`, `compression`), so a plugin sees the upstream's
/// response and its mutations are still subject to those middlewares.
pub(super) fn apply_response_phases(
    middlewares: &[MiddleWares],
    info: PhaseInfo<'_>,
    response: &mut Response<BoxBody<Bytes, AppError>>,
) -> Result<(), AppError> {
    if !has_wasm(middlewares) {
        return Ok(());
    }
    for middleware in middlewares.iter() {
        let denial = {
            let mut phase = ResponsePhase {
                route_id: info.route_id,
                request_id: info.request_id,
                response: &mut *response,
            };
            middleware.on_response_phase(&mut phase)?
        };
        if let Some(denial) = denial {
            *response = denial_response(denial)?;
        }
    }
    Ok(())
}

/// Calls `on-log` on every plugin of the route. Never fails the request: the
/// client's response is already committed at this point.
pub(super) fn apply_log_phases(
    middlewares: &[MiddleWares],
    info: PhaseInfo<'_>,
    method: &str,
    path: &str,
    response: &Response<BoxBody<Bytes, AppError>>,
    total: Duration,
) {
    if !has_wasm(middlewares) {
        return;
    }
    let status = response.status().as_u16();
    // Spire answers 503 (concurrency limit) and 504 (upstream timeout) itself;
    // those are gateway facts, not upstream facts.
    let local = matches!(status, 503 | 504);
    // The upstream latency is already exported as `x-upstream-latency` on the
    // response, so it does not need a second plumbing path.
    let upstream_millis = response
        .headers()
        .get("x-upstream-latency")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    let summary = ExchangeSummary {
        request_id: info.request_id,
        route_id: info.route_id,
        method,
        path,
        status,
        local,
        total_millis: total.as_millis() as u64,
        upstream_millis,
        // Bodies are streamed, so their size is not known here. `None` is the
        // honest answer; a plugin that needs sizes must opt into body mode.
        request_bytes: None,
        response_bytes: None,
    };
    for middleware in middlewares {
        middleware.on_log_phase(&summary);
    }
}
