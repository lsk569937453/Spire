use configuration_service::logger::setup_logger;
#[cfg(not(target_env = "msvc"))]
use mimalloc::MiMalloc;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

extern crate derive_builder;
use crate::vojo::cli::Cli;
mod configuration_service;
mod constants;
mod middleware;
use clap::Parser;
mod control_plane;
use crate::vojo::cli::SharedConfig;
mod health_check;
use crate::constants::common_constants::DEFAULT_ADMIN_PORT;
use crate::vojo::app_config::AppConfig;
mod monitor;
mod proxy;
use tracing_subscriber::filter;
mod utils;
use tracing_subscriber::filter::LevelFilter;
#[macro_use]
extern crate tracing;

#[macro_use]
extern crate axum;
mod vojo;
use crate::configuration_service::app_config_service;
use crate::vojo::app_error::AppError;
#[macro_use]
extern crate log;
use crate::control_plane::rest_api::start_control_plane;
use tracing_subscriber::reload::Handle;

#[tokio::main]
async fn main() -> Result<(), AppError> {
    let reload_handle = setup_logger()?;

fn main() -> Result<(), AppError> {
    let num = num_cpus::get();
    let rt = runtime::Builder::new_multi_thread()
        .worker_threads(num * 2)
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        if let Err(e) = start().await {
            error!("start error: {:?}", e);
            eprint!("start error: {:?}", e)
        }
    });
    Ok(())
}
async fn start() -> Result<(), AppError> {
    let reload_handle = setup_logger().map_err(|e| AppError(e.to_string())).unwrap();

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
    let _ = reload_handle.modify(|filter| {
        *filter = filter::Targets::new()
            .with_targets(vec![
                ("delay_timer", LevelFilter::OFF),
                ("hyper_util", LevelFilter::OFF),
            ])
            .with_default(config.static_config.get_log_level())
    });

    let admin_port = config
        .static_config
        .admin_port
        .unwrap_or(DEFAULT_ADMIN_PORT);
    let shared_config = SharedConfig::from_app_config(config);

    app_config_service::init(shared_config.clone()).await?;
    info!("Configuration service initialized.");

    info!("Starting control plane on port {}...", admin_port);
    start_control_plane(admin_port, shared_config).await?;

    info!("Application shut down gracefully. ");
    Ok(())
}
#[cfg(test)]
mod tests {
    use crate::vojo::app_config::StaticConifg;

    use super::*;

    use std::collections::HashMap;

    // Test configuration
    fn test_config() -> AppConfig {
        AppConfig {
            static_config: StaticConifg {
                log_level: None,
                admin_port: Some(8080),
                access_log: None,
                database_url: None,
                config_file_path: None, // other fields...
            },
            api_service_config: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn test_start_with_config_file() {
        let cli = Cli {
            config_path: "conf/app_config.yaml".to_string(),
        };
        let result = start().await;
        assert!(result.is_err());
    }
}
