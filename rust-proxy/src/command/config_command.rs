//! `spire config expand`: print the fully expanded configuration.
//!
//! The equivalent of `nginx -T`. Together with `spire validate` it is the only
//! way to see what the process would run with, now that the control-plane HTTP
//! API (and its `GET /appConfig`) is gone.
use crate::configuration_service::config_loader;
use crate::vojo::app_error::AppError;
use crate::vojo::cli::ExpandArgs;
use std::path::Path;
use std::path::PathBuf;

/// Prints the parsed configuration (defaults applied) to stdout.
///
/// Secrets are printed as well: this is a local debugging command, and
/// redacting them would defeat diffing two configurations against each other.
pub async fn handle_config_expand_command(
    args: ExpandArgs,
    default_path: &str,
) -> Result<(), AppError> {
    let path = PathBuf::from(args.config.as_deref().unwrap_or(default_path));
    let expanded = expand_config(&path).await?;
    print!("{expanded}");
    Ok(())
}

pub async fn expand_config(path: &Path) -> Result<String, AppError> {
    let config = config_loader::read_app_config(path).await?;
    Ok(serde_yaml::to_string(&config)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn expand_applies_defaults_and_keeps_routes() {
        let mut file = NamedTempFile::new().expect("temp file");
        write!(
            file,
            "servers:\n  - listen: 8080\n    protocol: http\n    routes:\n      - route_id: api\n        matchers:\n          - kind: path\n            value: /\n        forward_to: http://backend:8080\n"
        )
        .expect("write");

        let expanded = expand_config(file.path()).await.expect("expand");

        assert!(expanded.contains("listen: 8080"), "{expanded}");
        assert!(expanded.contains("route_id: api"), "{expanded}");
        // Defaults are materialised, which is the point of `-T`-style output.
        assert!(expanded.contains("observability"), "{expanded}");
    }

    #[tokio::test]
    async fn expand_rejects_a_removed_field() {
        let mut file = NamedTempFile::new().expect("temp file");
        write!(file, "admin_port: 8870\nservers: []\n").expect("write");

        let error = expand_config(file.path()).await.expect_err("removed field");

        assert!(error.0.contains("admin_port"), "{}", error.0);
    }
}
