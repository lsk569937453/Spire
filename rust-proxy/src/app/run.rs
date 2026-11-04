use crate::configuration_service::logger::setup_logger;
#[cfg(not(target_env = "msvc"))]
use mimalloc::MiMalloc;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

extern crate derive_builder;
use crate::command::examples_command::handle_examples_command;
use crate::command::openapi_converter::handle_convert_command;
use crate::command::query_command::handle_query_command;
use crate::command::reload_command::handle_reload_command;
use crate::command::validate_command::handle_validate_command;
use crate::monitor::prometheus_exporter::metrics;
use crate::vojo::cli::Cli;

use clap::Parser;

use crate::constants::common_constants::DEFAULT_ADMIN_PORT;
use crate::vojo::app_config::AppConfig;
use crate::vojo::cli::SharedConfig;

use crate::vojo::cli::Commands;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::{Registry, filter};

use crate::configuration_service::app_config_service;
use crate::vojo::app_error::AppError;

use crate::control_plane::rest_api::start_control_plane;
use tracing_subscriber::reload::Handle;

pub async fn main_with_error() -> Result<(), AppError> {
    #[cfg(feature = "pprof")]
    tokio::spawn(async {
        let profiler = pprof::ProfilerGuard::new(30).unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(20)).await;
        if let Ok(report) = profiler.report().build() {
            let file = std::fs::File::create("flamegraph.svg").unwrap();
            report.flamegraph(file).unwrap();
            println!("自动生成火焰图完成");
        };
    });
    let (reload_handle, _guard) = setup_logger()?;
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_e| AppError::from("Failed to install rustls crypto provider"))?;

    let cli = Cli::parse();
    info!("CLI arguments parsed: {cli:?}");

    match cli.command {
        Some(command) => match command {
            Commands::Convert(args) => {
                info!("'convert' command detected, starting conversion...");
                if let Err(e) = handle_convert_command(args).await {
                    error!("Failed to convert OpenAPI file: {e:?}");
                    return Err(e);
                }
                info!("Conversion successful!");
            }
            Commands::Examples(args) => {
                info!("'examples' command detected");
                if let Err(e) = handle_examples_command(args).await {
                    error!("Failed to show examples: {e}");
                    return Err(AppError::from(e.as_str()));
                }
            }
            Commands::Validate(args) => {
                info!("'validate' command detected");
                if let Err(e) = handle_validate_command(args).await {
                    error!("Validation failed: {e}");
                    // Exit with error code 1 for invalid config
                    std::process::exit(1);
                }
                // Exit with success code
                std::process::exit(0);
            }
            Commands::Query(args) => {
                info!("'query' command detected");
                if let Err(e) = handle_query_command(args).await {
                    error!("Query failed: {e}");
                    return Err(AppError(e));
                }
                info!("Query successful!");
                std::process::exit(0);
            }
            Commands::Reload(args) => {
                info!("'reload' command detected");
                if let Err(e) = handle_reload_command(args).await {
                    error!("Reload failed: {e}");
                    return Err(AppError(e));
                }
                info!("Reload successful!");
                std::process::exit(0);
            }
        },
        None => {
            info!("No subcommand specified, starting Spire gateway...");

            if let Err(e) = run_app(cli, reload_handle).await {
                error!("Application failed to start: {e:?}");
                return Err(e);
            }
        }
    }

    Ok(())
}

async fn run_app(
    cli: Cli,
    reload_handle: Handle<filter::Targets, Registry>,
) -> Result<(), AppError> {
    let config = load_config(&cli).await?;
    info!("Configuration loaded successfully.");
    // Never dump the configuration at default log levels: it contains TLS keys,
    // API keys and JWT secrets.
    debug!("Full configuration: {config:?}");
    reconfigure_logger(&reload_handle, &config);
    info!("Logger reconfigured to level: {}", config.get_log_level());
    metrics::init().map_err(|e| AppError(e.to_string()))?;
    let admin_port = config.admin_port.unwrap_or(DEFAULT_ADMIN_PORT);
    let shared_config = SharedConfig::from_app_config_with_path(config, &cli.config_path);

    app_config_service::init(shared_config.clone()).await?;
    info!("Configuration service initialized.");

    info!("Starting control plane on port {admin_port}...");
    start_control_plane(admin_port, shared_config.clone(), shutdown_signal()).await?;

    // The control plane has stopped: ask every proxy listener to stop as well.
    shutdown_listeners(&shared_config).await;

    info!("Application shut down gracefully. ");
    Ok(())
}

/// Waits for `SIGINT` (Ctrl+C) or `SIGTERM` (docker stop / k8s rollout).
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            error!("Failed to listen for Ctrl+C: {e}");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(e) => {
                error!("Failed to listen for SIGTERM: {e}");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => debug!("Ctrl+C received"),
        _ = terminate => debug!("SIGTERM received"),
    }
    info!("Shutdown signal received, draining listeners...");
}

/// Stops every proxy listener through its shutdown channel.
async fn shutdown_listeners(shared_config: &SharedConfig) {
    let senders = {
        let Ok(config) = shared_config.shared_data.read() else {
            return;
        };
        config
            .api_service_config
            .values()
            .map(|service| service.sender.clone())
            .collect::<Vec<_>>()
    };

    for sender in senders {
        // The receivers stop accepting as soon as the control plane returns.
        let _ = sender.send(()).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
}

async fn load_config(cli: &Cli) -> Result<AppConfig, AppError> {
    let config_str = tokio::fs::read_to_string(&cli.config_path)
        .await
        .map_err(|e| AppError(format!("Failed to read '{}': {e}", cli.config_path)))?;
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
        ("h2", LevelFilter::OFF),
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
    use std::io::Write;

    use tempfile::NamedTempFile;
    use tracing::Level;
    use tracing_subscriber::registry;
    use tracing_subscriber::reload;

    #[tokio::test]
    #[ignore]
    async fn test_start_with_config_file_integration() {
        let cli = Cli {
            config_path: "conf/app_config.yaml".to_string(),
            command: None,
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
                println!("Skipping test: config/examples directory not found. Error: {e}");
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
                    let error_msg = format!("Failed to parse config file '{file_path_str}': {e}");
                    eprintln!("{error_msg}");
                    AppError(error_msg)
                })?;
            }
        }
        Ok(())
    }
    fn create_temp_config_file(content: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().expect("Failed to create temp file");
        write!(file, "{content}").expect("Failed to write to temp file");
        file
    }

    fn setup_test_logger_handle() -> Handle<filter::Targets, registry::Registry> {
        let filter = filter::Targets::new().with_default(Level::INFO);
        let (_, reload_handle) = reload::Layer::new(filter);

        reload_handle
    }
    #[tokio::test]
    async fn test_load_config_success() {
        let yaml_content = r#"
log_level: info
servers:
  - listen: 8084
    protocol: http
    routes:
      - match:
          prefix: /
        forward_to: http://192.168.0.0:9393
        "#;
        let config_file = create_temp_config_file(yaml_content);
        let config_path_str = config_file.path().to_str().unwrap();
        let cli = Cli::try_parse_from(vec!["spire", "-f", config_path_str]);
        println!("cli: {cli:?}");
        assert!(cli.is_ok());
        let cli = cli.unwrap();

        let result = load_config(&cli).await;

        println!("result: {result:?}");
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_load_config_invalid_yaml() {
        let invalid_yaml = "log_level: 'debug'\ninvalid-yaml-format";
        let config_file = create_temp_config_file(invalid_yaml);
        let cli = Cli::try_parse_from(&config_file.path().to_path_buf());
        assert!(cli.is_err());
    }

    #[test]
    fn test_reconfigure_logger_with_health_check_logs_disabled() {
        let reload_handle = setup_test_logger_handle();
        let config = AppConfig {
            admin_port: None,
            health_check_log_enabled: Some(false),
            ..Default::default()
        };

        reconfigure_logger(&reload_handle, &config);

        let res = reload_handle.with_current(|filter| {
            let filter_str = filter.to_string();

            assert!(filter_str.contains("info"));
            assert!(filter_str.contains("delay_timer=off"));
            assert!(filter_str.contains("hyper_util=off"));
            assert!(filter_str.contains("spire::health_check::health_check_task=off"));
        });
        assert!(res.is_err());
    }

    #[test]
    fn test_reconfigure_logger_with_health_check_logs_enabled() {
        let reload_handle = setup_test_logger_handle();
        let config = AppConfig {
            admin_port: None,
            health_check_log_enabled: Some(true),
            ..Default::default()
        };

        reconfigure_logger(&reload_handle, &config);

        let res = reload_handle.with_current(|filter| {
            let filter_str = filter.to_string();
            assert!(filter_str.contains("debug"));
            assert!(filter_str.contains("delay_timer=off"));
            assert!(filter_str.contains("hyper_util=off"));
            assert!(!filter_str.contains("spire::health_check::health_check_task=off"));
        });
        assert!(res.is_err());
    }

    #[test]
    fn test_reconfigure_logger_with_health_check_logs_not_set() {
        let reload_handle = setup_test_logger_handle();
        let config = AppConfig {
            admin_port: None,
            health_check_log_enabled: None, // 关键测试点
            ..Default::default()
        };

        reconfigure_logger(&reload_handle, &config);

        let res = reload_handle.with_current(|filter| {
            let filter_str = filter.to_string();
            assert!(filter_str.contains("warn"));
            assert!(filter_str.contains("delay_timer=off"));
            assert!(filter_str.contains("hyper_util=off"));
            assert!(filter_str.contains("spire::health_check::health_check_task=off"));
        });
        assert!(res.is_err());
    }
}
