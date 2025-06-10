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
use tracing_subscriber::{filter, Registry};
mod utils;
use tracing_subscriber::filter::LevelFilter;

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

    if let Err(e) = run_app(reload_handle).await {
        error!("Application failed to start: {:?}", e);
        return Err(e);
    }

    Ok(())
}

async fn run_app(reload_handle: Handle<filter::Targets, Registry>) -> Result<(), AppError> {
    let cli = Cli::parse();
    info!("CLI arguments parsed: {:?}", cli);

    let config = load_config(&cli).await?;
    info!("Configuration loaded successfully.");
    println!("Full configuration: {:?}", config);

    reconfigure_logger(&reload_handle, &config);
    info!("Logger reconfigured to level: {}", config.get_log_level());

    let admin_port = config.admin_port.unwrap_or(DEFAULT_ADMIN_PORT);
    let shared_config = SharedConfig::from_app_config(config);

    app_config_service::init(shared_config.clone()).await?;
    info!("Configuration service initialized.");

    info!("Starting control plane on port {}...", admin_port);
    start_control_plane(admin_port, shared_config).await?;

    info!("Application shut down gracefully.");
    Ok(())
}

async fn load_config(cli: &Cli) -> Result<AppConfig, AppError> {
    let config_str = tokio::fs::read_to_string(&cli.config_path).await?;
    let config: AppConfig = serde_yaml::from_str(&config_str)?;
    Ok(config)
}

fn reconfigure_logger(
    reload_handle: &Handle<filter::Targets, Registry>,
    static_config: &AppConfig,
) {
    let mut targets = vec![
        ("delay_timer", LevelFilter::OFF),
        ("hyper_util", LevelFilter::OFF),
    ];

    if !static_config.health_check_log_enabled.unwrap_or(false) {
        targets.push(("spire::health_check::health_check_task", LevelFilter::OFF));
    }

    let _ = reload_handle.modify(|filter| {
        *filter = filter::Targets::new()
            .with_targets(targets)
            .with_default(static_config.get_log_level());
    });
}

#[cfg(test)]
mod tests {

    use super::*;

    #[tokio::test]
    #[ignore]
    async fn test_start_with_config_file_integration() {
        let cli = Cli {
            config_path: "conf/app_config.yaml".to_string(),
        };

        let config_result = load_config(&cli).await;

        assert!(
            config_result.is_ok(),
            "Should be able to load the main config file"
        );
    }

    #[tokio::test]
    async fn test_config_examples_are_valid() -> Result<(), AppError> {
        let paths = match std::fs::read_dir("config/examples") {
            Ok(paths) => paths,
            Err(e) => {
                println!(
                    "Skipping test: config/examples directory not found. Error: {}",
                    e
                );
                return Ok(());
            }
        };

        for path_result in paths {
            let path = path_result?.path();
            if path.extension().and_then(|s| s.to_str()) == Some("yaml") {
                let file_path_str = path.display().to_string();
                trace!("Testing config file: {}", &file_path_str);

                let config_str = tokio::fs::read_to_string(&path).await?;

                serde_yaml::from_str::<AppConfig>(&config_str).map_err(|e| {
                    let error_msg =
                        format!("Failed to parse config file '{}': {}", file_path_str, e);
                    eprintln!("{}", error_msg);
                    AppError(error_msg)
                })?;
            }
        }
        Ok(())
    }
}
