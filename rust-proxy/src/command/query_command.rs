use crate::vojo::cli::QueryArgs;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;

const APP_CONFIG_ENDPOINT: &str = "/appConfig";

pub async fn handle_query_command(args: QueryArgs) -> Result<(), String> {
    let url = format!("http://{}:{}{}", args.host, args.port, APP_CONFIG_ENDPOINT);

    eprintln!("Querying configuration from control plane at {}", url);

    // Create HTTP client
    let client = Client::builder(TokioExecutor::new())
        .http1_title_case_headers(true)
        .http1_preserve_header_case(true)
        .build_http();

    // Build GET request
    let mut builder = Request::builder().method(hyper::Method::GET).uri(&url);
    if let Some(token) = args.token.as_deref() {
        builder = builder.header("Authorization", format!("Bearer {token}"));
    }
    let request = builder
        .body(Full::new(Bytes::new()))
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
        println!("{}", response_text);
        Ok(())
    } else {
        Err(format!(
            "Query failed with status {}: {}",
            status, response_text
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_url_format() {
        let args = QueryArgs {
            port: 8081,
            host: "127.0.0.1".to_string(),
            format: "yaml".to_string(),
            token: None,
        };
        let expected = "http://127.0.0.1:8081/appConfig";
        let actual = format!("http://{}:{}{}", args.host, args.port, APP_CONFIG_ENDPOINT);
        assert_eq!(actual, expected);
    }
}
