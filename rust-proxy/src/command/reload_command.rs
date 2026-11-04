use crate::vojo::cli::ReloadArgs;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use std::fs;

const RELOAD_ENDPOINT: &str = "/reload";

pub async fn handle_reload_command(args: ReloadArgs) -> Result<(), String> {
    // Read the config file
    let content = fs::read_to_string(&args.config).map_err(|e| {
        format!(
            "Failed to read config file '{}': {}",
            args.config,
            e.kind().to_string().to_lowercase()
        )
    })?;

    eprintln!(
        "Reloading configuration from '{}' to control plane at {}:{}",
        args.config, args.host, args.port
    );

    let url = format!("http://{}:{}{}", args.host, args.port, RELOAD_ENDPOINT);

    // Create HTTP client
    let client = Client::builder(TokioExecutor::new())
        .http1_title_case_headers(true)
        .http1_preserve_header_case(true)
        .build_http();

    // Build POST request
    let mut builder = Request::builder()
        .method(hyper::Method::POST)
        .uri(&url)
        .header("Content-Type", "application/yaml");
    if let Some(token) = args.token.as_deref() {
        builder = builder.header("Authorization", format!("Bearer {token}"));
    }
    let request = builder
        .body(Full::new(Bytes::from(content)))
        .map_err(|e| format!("Failed to build request: {}", e))?;

    // Send request
    let response = client
        .request(request)
        .await
        .map_err(|e| format!("Failed to connect to control plane: {}", e))?;

    let status = response.status();

    // Collect response body
    let body = response
        .into_body()
        .collect()
        .await
        .map_err(|e| format!("Failed to read response: {}", e))?
        .to_bytes();
    let response_text = String::from_utf8_lossy(&body);

    if status.is_success() {
        println!("Configuration reloaded successfully!");
        Ok(())
    } else {
        Err(format!(
            "Reload failed with status {}: {}",
            status, response_text
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reload_url_format() {
        let args = ReloadArgs {
            port: 8081,
            host: "127.0.0.1".to_string(),
            config: "config.yaml".to_string(),
            token: None,
        };
        let expected = "http://127.0.0.1:8081/reload";
        let actual = format!("http://{}:{}{}", args.host, args.port, RELOAD_ENDPOINT);
        assert_eq!(actual, expected);
    }
}
