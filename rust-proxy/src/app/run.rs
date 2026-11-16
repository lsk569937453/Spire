use crate::configuration_service::logger::setup_logger;
#[cfg(not(target_env = "msvc"))]
use mimalloc::MiMalloc;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

extern crate derive_builder;
use crate::command::config_command::handle_config_expand_command;
use crate::command::examples_command::handle_examples_command;
use crate::command::openapi_converter::handle_convert_command;
use crate::command::reload_command::handle_reload_command;
use crate::command::validate_command::handle_validate_command;
use crate::monitor::prometheus_exporter::metrics;
use crate::vojo::cli::Cli;

use clap::Parser;

use crate::configuration_service::config_loader;
use crate::proxy::concurrency;
use crate::utils::memory_limit;
use crate::vojo::app_config::AppConfig;
use crate::vojo::cli::SharedConfig;

use crate::vojo::cli::Commands;
use crate::vojo::cli::ConfigCommand;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::{Registry, filter};

use crate::configuration_service::app_config_service;
use crate::vojo::app_error::AppError;

use crate::observability::server::start_observability;
use tracing_subscriber::reload::Handle;

#[cfg(feature = "pprof")]
fn profile_env<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

pub async fn main_with_error() -> Result<(), AppError> {
    #[cfg(feature = "pprof")]
    tokio::spawn(async {
        // Profiling is a diagnostic side channel: a failure here must never
        // take the process (or the request path) down with it.
        //
        // Sampling is deferred so that a benchmark can warm up first:
        //   SPIRE_PROFILE_DELAY_SECS - idle seconds before sampling (default 5)
        //   SPIRE_PROFILE_SECS       - sampling window in seconds (default 60)
        //   SPIRE_PROFILE_HZ         - sampling frequency in Hz (default 99)
        //   SPIRE_PROFILE_OUT        - flamegraph output path (default flamegraph.svg)
        let delay_secs: u64 = profile_env("SPIRE_PROFILE_DELAY_SECS", 5);
        let window_secs: u64 = profile_env("SPIRE_PROFILE_SECS", 60);
        let hz: i32 = profile_env("SPIRE_PROFILE_HZ", 99);
        let out_path: String = profile_env("SPIRE_PROFILE_OUT", "flamegraph.svg".to_string());
        tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
        let Ok(profiler) = pprof::ProfilerGuard::new(hz) else {
            eprintln!("pprof: failed to start the profiler");
            return;
        };
        tokio::time::sleep(std::time::Duration::from_secs(window_secs)).await;
        if let Ok(report) = profiler.report().build() {
            let Ok(file) = std::fs::File::create(&out_path) else {
                eprintln!("pprof: failed to create {out_path}");
                return;
            };
            if report.flamegraph(file).is_ok() {
                println!("Flamegraph written to {out_path}");
            }
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
                    // A CLI validation failure must be visible on the console,
                    // not only in the log file: this is the command CI runs.
                    // Exit with error code 1 for invalid config.
                    eprintln!("[ERROR] {e}");
                    std::process::exit(1);
                }
                // Exit with success code
                std::process::exit(0);
            }
            Commands::Config(args) => {
                info!("'config' command detected");
                match args.command {
                    ConfigCommand::Expand(expand_args) => {
                        if let Err(e) =
                            handle_config_expand_command(expand_args, &cli.config_path).await
                        {
                            error!("Failed to expand configuration: {e}");
                            return Err(e);
                        }
                    }
                }
                std::process::exit(0);
            }
            Commands::Reload(args) => {
                info!("'reload' command detected");
                if let Err(e) = handle_reload_command(args) {
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
    // Size the concurrency memory guard from the memory this process is
    // actually allowed to use (cgroup limit in containers, host RAM on bare
    // metal) — unless the operator configured an explicit limit.
    if config.upstream_concurrency_limit.is_none() {
        let (mem_bytes, source) = match memory_limit::detect_memory_limit_bytes() {
            Some((bytes, source)) => (bytes, source.as_str()),
            None => (0, "not detected, using conservative defaults"),
        };
        let derived = concurrency::init_derived_limits((mem_bytes > 0).then_some(mem_bytes));
        metrics::set_memory_limit_bytes(mem_bytes as f64);
        metrics::set_upstream_global_permits(derived.global_permits as f64);
        info!(
            "[ConcurrencyLimit] derived from memory: {} ({source}); budget {:.0}% => {}B/request; \
             global memory guard = {} permits, endpoint default limit = {}",
            mem_bytes,
            derived.budget_fraction * 100.0,
            derived.per_request_estimate_bytes,
            derived.global_permits,
            derived.endpoint_default,
        );
    } else {
        info!(
            "[ConcurrencyLimit] upstream_concurrency_limit configured explicitly: {}; \
             global memory guard stays at the conservative default",
            config.upstream_concurrency_limit.unwrap_or_default()
        );
    }
    let shared_config = SharedConfig::from_app_config_with_path(config, &cli.config_path);
    info!(
        "Configuration file: {}",
        shared_config.config_path.display()
    );

    app_config_service::init(shared_config.clone()).await?;
    info!("Configuration service initialized.");

    // WASM plugins are compiled before the listeners start: a compile must
    // delay startup, never a request. The directory is process-wide and fixed
    // for the lifetime of the process (changing it requires a restart); what
    // changes on reload is which packages it contains.
    #[cfg(feature = "wasm")]
    {
        let plugins_dir = shared_config
            .shared_data
            .read()?
            .plugins_dir
            .clone()
            .map(std::path::PathBuf::from);
        crate::wasm::init(plugins_dir)?;
        let loaded = crate::wasm::loaded_ids();
        if loaded.is_empty() {
            info!(
                "WASM plugin system ready (no plugins in {})",
                crate::wasm::default_dir().display()
            );
        } else {
            info!(
                "WASM plugin system ready: {} plugin(s) loaded: {}",
                loaded.len(),
                loaded.join(", ")
            );
        }
        {
            let config = shared_config.shared_data.read()?;
            crate::wasm::validate_references(&config)?;
        }
    }

    // The observability listener is read-only and bound once: a SIGHUP reload
    // keeps it (changing its address requires a restart).
    let (observability_shutdown_tx, observability_shutdown_rx) = tokio::sync::oneshot::channel();
    let observability_config = shared_config.shared_data.read()?.observability.clone();
    let observability_server = start_observability(&observability_config, async move {
        let _ = observability_shutdown_rx.await;
    })
    .await?;

    // The process lives in this loop: SIGHUP reloads the configuration from
    // disk, SIGINT/SIGTERM shut everything down.
    wait_for_signals(&shared_config).await;

    // Ask the observability listener to stop, then every proxy listener.
    let _ = observability_shutdown_tx.send(());
    if let Some(handle) = observability_server {
        let waited = tokio::time::timeout(
            std::time::Duration::from_secs(crate::constants::common_constants::TIMER_WAIT_SECONDS),
            handle,
        )
        .await;
        if waited.is_err() {
            warn!("The observability listener did not stop in time.");
        }
    }
    shutdown_listeners(&shared_config).await;

    // Tear down this thread's plugin instances, so `lifecycle.stop` runs while
    // the runtime is still healthy instead of during process teardown.
    #[cfg(feature = "wasm")]
    crate::wasm::shutdown();

    info!("Application shut down gracefully. ");
    Ok(())
}

/// Waits for a shutdown signal, reloading the configuration on `SIGHUP`.
///
/// A failed reload only logs: the process keeps serving with the configuration
/// it already has, which is also what nginx does. `SIGHUP` does not exist on
/// Windows, so a reload there means a restart.
async fn wait_for_signals(shared_config: &SharedConfig) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::SignalKind;
        use tokio::signal::unix::signal;

        let mut interrupt = match signal(SignalKind::interrupt()) {
            Ok(signal) => signal,
            Err(e) => {
                error!("Failed to listen for SIGINT: {e}");
                std::future::pending::<()>().await;
                return;
            }
        };
        let mut terminate = match signal(SignalKind::terminate()) {
            Ok(signal) => signal,
            Err(e) => {
                error!("Failed to listen for SIGTERM: {e}");
                std::future::pending::<()>().await;
                return;
            }
        };
        let mut hangup = match signal(SignalKind::hangup()) {
            Ok(signal) => signal,
            Err(e) => {
                error!("Failed to listen for SIGHUP: {e}");
                std::future::pending::<()>().await;
                return;
            }
        };

        loop {
            tokio::select! {
                _ = interrupt.recv() => {
                    debug!("SIGINT received");
                    break;
                }
                _ = terminate.recv() => {
                    debug!("SIGTERM received");
                    break;
                }
                _ = hangup.recv() => {
                    info!(
                        "SIGHUP received, reloading configuration from '{}'",
                        shared_config.config_path.display()
                    );
                    match app_config_service::reload_from_disk(shared_config).await {
                        Ok(()) => info!("Configuration reloaded."),
                        Err(e) => error!(
                            "Configuration reload failed, keeping the previous configuration: {e}"
                        ),
                    }
                }
            }
        }
    }

    #[cfg(not(unix))]
    {
        let _ = shared_config;
        if let Err(e) = tokio::signal::ctrl_c().await {
            error!("Failed to listen for Ctrl+C: {e}");
            std::future::pending::<()>().await;
        }
        debug!("Ctrl+C received");
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
        let _ = sender.send(()).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
}

async fn load_config(cli: &Cli) -> Result<AppConfig, AppError> {
    config_loader::read_app_config(std::path::Path::new(&cli.config_path)).await
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
            health_check_log_enabled: None, // key assertion of this test
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
