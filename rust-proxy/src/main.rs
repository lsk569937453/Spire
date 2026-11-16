mod app;
mod command;
mod configuration_service;
mod constants;
mod health_check;
mod middleware;
mod monitor;
mod observability;
mod proxy;
mod tls;
mod utils;
mod vojo;
#[cfg(feature = "wasm")]
mod wasm;
use crate::app::run::main_with_error;
#[macro_use]
extern crate clap;
#[macro_use]
extern crate tracing;
#[tokio::main]
async fn main() {
    if let Err(e) = main_with_error().await {
        // A fatal startup error must be visible to supervisors (systemd,
        // docker, kubernetes) — printing and returning normally used to exit
        // with status 0, so an orchestrator would consider a dead gateway
        // "healthy" and never restart it.
        eprintln!("Fatal error: {e}");
        std::process::exit(1);
    }
}
