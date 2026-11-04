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
    let _config: AppConfig = serde_yaml::from_str(&content).map_err(|e| {
        format!(
            "Invalid YAML syntax in '{}':\n  {}",
            config_path,
            format_yaml_error(&e)
        )
    })?;

    if args.verbose {
        eprintln!("Configuration parsed successfully!");
    }

    println!("[OK] Configuration file '{}' is valid!", config_path);

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
