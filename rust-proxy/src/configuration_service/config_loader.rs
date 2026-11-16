//! The single place that turns configuration bytes into an [`AppConfig`].
//!
//! `serve`, `validate`, `config expand` and the SIGHUP reload must all apply the
//! same rules: a configuration that validates in CI but fails on reload (or the
//! other way round) is worse than either failure on its own.
//!
//! Every error carries the file it came from, because a reload failure is only
//! actionable if the operator knows which file to edit.
use crate::vojo::app_config::AppConfig;
use crate::vojo::app_error::AppError;
use std::path::Path;

/// Top-level keys that belonged to the removed control-plane HTTP API.
///
/// They are rejected explicitly instead of being silently ignored: a
/// configuration that still sets them would otherwise start with no management
/// surface at all and no hint why.
const REMOVED_KEYS: [(&str, &str); 3] = [
    (
        "admin_port",
        "the control-plane HTTP API was removed; the read-only observability listener is configured with 'observability.listen'",
    ),
    (
        "admin_listen",
        "the control-plane HTTP API was removed; the read-only observability listener is configured with 'observability.listen'",
    ),
    (
        "admin_token",
        "the control-plane HTTP API was removed; the observability listener is read-only and needs no token",
    ),
];

/// Reads and parses the configuration at `path`.
pub async fn read_app_config(path: &Path) -> Result<AppConfig, AppError> {
    let content = tokio::fs::read_to_string(path)
        .await
        .map_err(|e| AppError(format!("Failed to read '{}': {e}", path.display())))?;
    parse_app_config(path, &content)
}

/// Parses `content`, reporting errors against `source`.
pub fn parse_app_config(source: &Path, content: &str) -> Result<AppConfig, AppError> {
    // Parsed twice on purpose: the first pass keeps the YAML locations for
    // syntax errors and lets the removed keys be checked, the second pass keeps
    // the locations for type errors.
    let value: serde_yaml::Value = serde_yaml::from_str(content).map_err(|e| {
        AppError(format!(
            "Invalid YAML in '{}': {}",
            source.display(),
            format_yaml_error(&e)
        ))
    })?;
    reject_removed_keys(source, &value)?;
    serde_yaml::from_str(content).map_err(|e| {
        AppError(format!(
            "Invalid configuration in '{}': {}",
            source.display(),
            format_yaml_error(&e)
        ))
    })
}

fn reject_removed_keys(source: &Path, value: &serde_yaml::Value) -> Result<(), AppError> {
    let serde_yaml::Value::Mapping(mapping) = value else {
        return Ok(());
    };
    for (key, hint) in REMOVED_KEYS {
        if mapping.contains_key(serde_yaml::Value::String(key.to_string())) {
            return Err(AppError(format!(
                "'{}' still sets '{key}', which was removed: {hint}",
                source.display()
            )));
        }
    }
    Ok(())
}

/// Formats a YAML error with its line and column, for both the CLI and the logs.
pub fn format_yaml_error(error: &serde_yaml::Error) -> String {
    let err_str = error.to_string();
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
    use std::path::PathBuf;

    fn source() -> PathBuf {
        PathBuf::from("config.yaml")
    }

    #[test]
    fn valid_config_parses() {
        let yaml = r#"
log_level: info
servers:
  - listen: 8080
    protocol: http
    routes:
      - route_id: api
        matchers:
          - kind: path
            value: /
        forward_to: http://backend:8080
"#;
        let config = parse_app_config(&source(), yaml).expect("valid configuration");
        assert!(config.api_service_config.contains_key(&8080));
    }

    #[test]
    fn removed_admin_port_is_rejected_with_a_hint() {
        let yaml = "admin_port: 8870\nservers: []\n";
        let error = parse_app_config(&source(), yaml).expect_err("admin_port is removed");
        let message = error.0;
        assert!(message.contains("admin_port"), "{message}");
        assert!(message.contains("observability.listen"), "{message}");
    }

    #[test]
    fn removed_admin_token_is_rejected() {
        let yaml = "admin_token: secret\nservers: []\n";
        assert!(parse_app_config(&source(), yaml).is_err());
    }

    #[test]
    fn parse_errors_carry_the_file_name() {
        let error = parse_app_config(&source(), "servers: [").expect_err("invalid yaml");
        assert!(error.0.contains("config.yaml"), "{}", error.0);
    }

    #[test]
    fn format_yaml_error_mentions_the_line() {
        let error = serde_yaml::from_str::<serde_yaml::Value>("invalid: yaml: content: [")
            .expect_err("invalid yaml");
        assert!(format_yaml_error(&error).contains("line"));
    }
}
