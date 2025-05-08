use configuration_service::logger::setup_logger;
#[cfg(not(target_env = "msvc"))]
use jemallocator::Jemalloc;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

extern crate derive_builder;
use crate::vojo::cli::Cli;
mod configuration_service;
mod constants;
use clap::Parser;
mod control_plane;
use crate::vojo::cli::SharedConfig;
mod health_check;
use crate::vojo::app_config::AppConfig;
mod monitor;
mod proxy;
mod utils;
#[macro_use]
extern crate tracing;
#[macro_use]
extern crate anyhow;
#[macro_use]
extern crate axum;
mod vojo;
use crate::vojo::app_error::AppError;
#[macro_use]
extern crate log;
use crate::control_plane::rest_api::start_control_plane;

use tokio::runtime;

fn main() -> Result<(), anyhow::Error> {
    let num = num_cpus::get();
    let rt = runtime::Builder::new_multi_thread()
        .worker_threads(num * 2)
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        if let Err(e) = start().await {
            error!("start error: {:?}", e);
        }
    });
    Ok(())
}
async fn start() -> Result<(), AppError> {
    let _ = setup_logger();
    let cli = Cli::parse();
    info!("cli: {:?}", cli);
    println!("cli: {:?}", cli);
    let config_str = tokio::fs::read_to_string(cli.config_path)
        .await
        .map_err(|e| AppError(e.to_string()))?;
    let config: AppConfig =
        serde_yaml::from_str(&config_str).map_err(|e| AppError(e.to_string()))?;
    info!("config is {:?}", config);
    println!("config is {:?}", config);
    let admin_port = config.static_config.admin_port;
    let shared_config = SharedConfig::from_app_config(config);

    configuration_service::app_config_service::init(shared_config.clone()).await?;
    start_control_plane(admin_port, shared_config).await?;
    Ok(())
}
