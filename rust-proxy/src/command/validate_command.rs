use crate::vojo::app_config::AppConfig;
use crate::vojo::cli::ValidateArgs;
use std::fs;

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

    // Parse and deserialize the YAML
    let config: AppConfig = serde_yaml::from_str(&content).map_err(|e| {
        format!(
            "Invalid YAML syntax in '{}':\n  {}",
            config_path,
            format_yaml_error(&e)
        )
    })?;

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

fn format_yaml_error(error: &serde_yaml::Error) -> String {
    let err_str = error.to_string();
    // Clean up the error message for better readability
    if let Some(line_col) = error.location() {
        format!(
            "{} at line {}, column {}",
            err_str.replace("EOF while parsing a value", "unexpected end of file"),
            line_col.line(),
            line_col.column()
        )
    } else {
        err_str
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_yaml_error() {
        let yaml = "invalid: yaml: content: [";
        let error = serde_yaml::from_str::<serde_yaml::Value>(yaml).unwrap_err();
        let formatted = format_yaml_error(&error);
        assert!(formatted.contains("line"));
    }

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
        let result: Result<AppConfig, _> = serde_yaml::from_str(yaml);
        assert!(
            result.is_ok(),
            "Valid config should deserialize successfully"
        );
    }
}
