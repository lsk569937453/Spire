use crate::vojo::cli::ExamplesArgs;
use std::fs;
use std::path::PathBuf;

const EXAMPLES_DIR: &str = "config/examples";

/// Available examples with descriptions
const EXAMPLE_LIST: &[(&str, &str)] = &[
    (
        "app_config_simple",
        "Minimal configuration with single backend forwarding",
    ),
    (
        "app_config_https",
        "HTTPS proxy with TLS certificate configuration",
    ),
    (
        "admin_security",
        "Control plane token/loopback binding and trusted proxy configuration",
    ),
    (
        "http_weight_route",
        "Weight-based load balancing across multiple backends",
    ),
    (
        "http_random_route",
        "Random backend selection for load distribution",
    ),
    ("http_poll_route", "Round-robin (poll) load balancing"),
    (
        "http_header_based_route",
        "Header-based routing with text/regex/split matching",
    ),
    (
        "http_to_grpc",
        "HTTP to gRPC transcoding with proto descriptors",
    ),
    (
        "http_cors",
        "CORS (Cross-Origin Resource Sharing) configuration",
    ),
    (
        "health_check",
        "Active/passive health checking with automatic failover",
    ),
    (
        "circuit_breaker",
        "Circuit breaker pattern for fault tolerance",
    ),
    ("reverse_proxy", "Basic reverse proxy with forward headers"),
    ("tcp_proxy", "TCP layer proxy with IP filtering"),
    ("jwt_auth", "JWT authentication middleware"),
    (
        "matchers",
        "Advanced request matching (path/host/header/method)",
    ),
    (
        "middle_wares",
        "Multiple middleware combination (auth + rate limit + allow/deny + CORS)",
    ),
    (
        "ratelimit_token_bucket",
        "Token bucket rate limiting algorithm",
    ),
    (
        "ratelimit_fixed_window",
        "Fixed window rate limiting algorithm",
    ),
    ("request_headers", "Add/remove custom request headers"),
    ("static_file", "Static file serving with caching headers"),
    (
        "openapi_convert",
        "OpenAPI spec conversion to routing rules",
    ),
    (
        "forward_ip_examples",
        "Forward client IP via X-Real-IP and X-Forwarded-For headers",
    ),
    (
        "compression",
        "gzip / zstd response compression with size thresholds",
    ),
    ("ip_ban", "Automatic IP banning for abusive clients"),
    (
        "app_config_load_testing",
        "Configuration used for local load testing",
    ),
];

pub async fn handle_examples_command(args: ExamplesArgs) -> Result<(), String> {
    if args.list {
        list_examples();
    } else if let Some(name) = args.name {
        display_example(&name)?;
    } else {
        // Default: list examples
        list_examples();
    }
    Ok(())
}

fn list_examples() {
    println!("Available configuration examples:\n");
    println!("{:<5} {:<35} Description", "No.", "Example Name");
    println!("{}", "-".repeat(100));

    for (i, (name, desc)) in EXAMPLE_LIST.iter().enumerate() {
        println!("{:<5} {:<35} {}", i + 1, name, desc);
    }

    println!("\nUsage:");
    println!("  spire examples --list              List all examples");
    println!("  spire examples <name>              Display a specific example");
    println!("\nExample:");
    println!("  spire examples app_config_simple   Display simple config example");
    println!("  spire examples forward_ip_examples Display IP forwarding examples");
}

fn display_example(name: &str) -> Result<(), String> {
    let file_path = find_example_file(name)?;

    let content = fs::read_to_string(&file_path)
        .map_err(|e| format!("Failed to read example file: {}", e))?;

    println!("{}", content);
    println!("\n---");
    println!("File: {}", file_path.display());

    Ok(())
}

fn find_example_file(name: &str) -> Result<PathBuf, String> {
    // Try exact name first
    let exact_path = PathBuf::from(EXAMPLES_DIR).join(format!("{}.yaml", name));
    if exact_path.exists() {
        return Ok(exact_path);
    }

    // Try with _examples suffix
    let with_suffix = PathBuf::from(EXAMPLES_DIR).join(format!("{}_examples.yaml", name));
    if with_suffix.exists() {
        return Ok(with_suffix);
    }

    // Search for partial match
    let examples_dir = PathBuf::from(EXAMPLES_DIR);
    if let Ok(entries) = fs::read_dir(&examples_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(file_name) = path.file_name().and_then(|n| n.to_str())
                && (file_name.starts_with(name) || file_name.contains(name))
            {
                return Ok(path);
            }
        }
    }

    Err(format!(
        "Example '{}' not found. Use --list to see available examples.",
        name
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_example_file_exact_match() {
        let result = find_example_file("app_config_simple");
        assert!(result.is_ok());
        let path = result.unwrap();
        assert!(path.to_str().unwrap().contains("app_config_simple.yaml"));
    }

    #[test]
    fn test_find_example_file_with_suffix() {
        let result = find_example_file("forward_ip");
        assert!(result.is_ok());
        let path = result.unwrap();
        assert!(path.to_str().unwrap().contains("forward_ip_examples.yaml"));
    }

    #[test]
    fn test_find_example_file_partial_match() {
        let result = find_example_file("weight");
        assert!(result.is_ok());
        let path = result.unwrap();
        assert!(path.to_str().unwrap().contains("http_weight_route.yaml"));
    }

    #[test]
    fn test_find_example_file_not_found() {
        let result = find_example_file("nonexistent_example");
        assert!(result.is_err());
    }
}
