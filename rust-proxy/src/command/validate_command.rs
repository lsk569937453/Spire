use crate::configuration_service::config_loader;
use crate::vojo::app_config::AppConfig;
use crate::vojo::cli::ValidateArgs;
use std::fs;
use std::path::Path;

pub async fn handle_validate_command(args: ValidateArgs) -> Result<(), String> {
    let config_path = args
        .config
        .clone()
        .unwrap_or_else(|| "config.yaml".to_string());

    if args.verbose {
        eprintln!("Validating configuration file: {}", config_path);
    }

    // Read the config file
    let content = fs::read_to_string(&config_path).map_err(|e| {
        format!(
            "Failed to read config file '{}': {}",
            config_path,
            e.kind().to_string().to_lowercase()
        )
    })?;

    if args.verbose {
        eprintln!("File read successfully, parsing configuration...");
    }

    // Parse and deserialize the YAML with the same loader the process and the
    // SIGHUP reload use, so a configuration cannot pass here and fail there.
    let config =
        config_loader::parse_app_config(Path::new(&config_path), &content).map_err(|e| e.0)?;

    // `kind: wasm` middlewares need a build that can run them and, for the
    // plugin itself, a package on disk. Reporting that here means `spire
    // validate` fails in CI instead of the data plane failing per request.
    validate_wasm_middlewares(&config)?;

    if args.verbose {
        eprintln!("Configuration parsed successfully!");
    }

    println!("[OK] Configuration file '{}' is valid!", config_path);

    Ok(())
}

/// Checks every `kind: wasm` middleware of the configuration.
///
/// Two distinct failures are reported separately, because they need different
/// fixes: budgets the host refuses (wrong config) and a plugin the host cannot
/// run (wrong binary, or the package is missing from the plugins directory).
fn validate_wasm_middlewares(config: &AppConfig) -> Result<(), String> {
    use crate::middleware::middlewares::MiddleWares;

    // Scanning the plugins directory is part of validating: a route that names a
    // plugin nobody installed must fail here, in CI, rather than on the first
    // request that reaches the route.
    #[cfg(feature = "wasm")]
    crate::wasm::init(config.plugins_dir.clone().map(std::path::PathBuf::from))
        .map_err(|e| format!("wasm plugin system: {}", e.0))?;

    for service in config.api_service_config.values() {
        for route in &service.route_configs {
            let Some(middlewares) = &route.middlewares else {
                continue;
            };
            for middleware in middlewares {
                let MiddleWares::Wasm(cfg) = middleware else {
                    continue;
                };
                let route_label = if route.route_id.is_empty() {
                    "<unnamed>"
                } else {
                    route.route_id.as_str()
                };
                cfg.validate()
                    .map_err(|e| format!("route {route_label}: {e}"))?;
                #[cfg(feature = "wasm")]
                crate::wasm::resolve(&cfg.plugin)
                    .map_err(|e| format!("route {route_label}: {}", e.0))?;
                #[cfg(not(feature = "wasm"))]
                return Err(format!(
                    "route {route_label} uses `kind: wasm`, but this build has no WASM plugin \
                     support; rebuild with `--features wasm`"
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_config_deserialization() {
        let yaml = r#"
log_level: info
servers:
  - listen: 8080
    protocol: http
    routes:
      - matchers:
          - kind: path
            value: /
        forward_to: http://backend:8080
"#;
        let result = config_loader::parse_app_config(Path::new("config.yaml"), yaml);
        assert!(
            result.is_ok(),
            "Valid config should deserialize successfully"
        );
    }

    #[test]
    fn test_removed_admin_field_is_reported() {
        let yaml = "admin_token: change-me\nservers: []\n";
        let error = config_loader::parse_app_config(Path::new("config.yaml"), yaml)
            .expect_err("admin_token was removed");
        assert!(error.0.contains("admin_token"), "{}", error.0);
    }
}
