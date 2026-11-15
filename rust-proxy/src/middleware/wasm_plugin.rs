//! Glue between the middleware layer (always compiled) and the WASM runtime
//! (behind the `wasm` feature).
//!
//! The split exists so that `kind: wasm` is a *parseable, validatable* part of
//! the configuration even in a build without `wasmtime`: the failure a user
//! sees is "this binary has no WASM support, rebuild with `--features wasm`"
//! rather than a silently ignored middleware.

use crate::middleware::middlewares::Denial;
use crate::middleware::middlewares::ExchangeSummary;
use crate::middleware::middlewares::RequestPhase;
use crate::middleware::middlewares::ResponsePhase;
use crate::middleware::wasm_plugin_config::WasmPluginConfig;
use crate::vojo::app_error::AppError;

#[cfg(not(feature = "wasm"))]
fn unsupported() -> AppError {
    AppError(
        "this build has no WASM plugin support (`kind: wasm`); rebuild with `--features wasm` or \
         remove the middleware"
            .to_string(),
    )
}

#[cfg(feature = "wasm")]
pub fn request_phase(
    cfg: &WasmPluginConfig,
    phase: &mut RequestPhase<'_>,
) -> Result<Option<Denial>, AppError> {
    crate::wasm::request_phase(cfg, phase)
}

#[cfg(feature = "wasm")]
pub fn response_phase(
    cfg: &WasmPluginConfig,
    phase: &mut ResponsePhase<'_>,
) -> Result<Option<Denial>, AppError> {
    crate::wasm::response_phase(cfg, phase)
}

#[cfg(feature = "wasm")]
pub fn log_phase(cfg: &WasmPluginConfig, summary: &ExchangeSummary<'_>) {
    crate::wasm::log_phase(cfg, summary);
}

#[cfg(not(feature = "wasm"))]
pub fn request_phase(
    _cfg: &WasmPluginConfig,
    _phase: &mut RequestPhase<'_>,
) -> Result<Option<Denial>, AppError> {
    Err(unsupported())
}

#[cfg(not(feature = "wasm"))]
pub fn response_phase(
    _cfg: &WasmPluginConfig,
    _phase: &mut ResponsePhase<'_>,
) -> Result<Option<Denial>, AppError> {
    Err(unsupported())
}

#[cfg(not(feature = "wasm"))]
pub fn log_phase(_cfg: &WasmPluginConfig, _summary: &ExchangeSummary<'_>) {}
