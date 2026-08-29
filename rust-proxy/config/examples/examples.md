# Spire Configuration Examples

This directory contains configuration examples for various Spire proxy features.

## Quick Start

```bash
# Run with any config file
spire -f config/examples/<example-file>.yaml
```

## Configuration Examples by Category

### Basic Configuration

| File | Description |
|------|-------------|
| [`app_config_simple.yaml`](app_config_simple.yaml) | Minimal configuration with single backend forwarding |
| [`app_config_https.yaml`](app_config_https.yaml) | HTTPS proxy with TLS certificate configuration |

### Routing Strategies

| File | Description |
|------|-------------|
| [`http_weight_route.yaml`](http_weight_route.yaml) | Weight-based load balancing across multiple backends |
| [`http_random_route.yaml`](http_random_route.yaml) | Random backend selection for load distribution |
| [`http_poll_route.yaml`](http_poll_route.yaml) | Round-robin (poll) load balancing |
| [`http_header_based_route.yaml`](http_header_based_route.yaml) | Header-based routing with text/regex/split matching |

### Middleware Features

| File | Description |
|------|-------------|
| [`forward_ip_examples.yaml`](forward_ip_examples.yaml) | Forward client IP via X-Real-IP and X-Forwarded-For headers |
| [`request_headers.yaml`](request_headers.yaml) | Add/remove custom request headers |
| [`http_cors.yaml`](http_cors.yaml) | CORS (Cross-Origin Resource Sharing) configuration |
| [`jwt_auth.yaml`](jwt_auth.yaml) | JWT authentication middleware |
| [`middle_wares.yaml`](middle_wares.yaml) | Multiple middleware combination (auth + rate limit + allow/deny + CORS) |

### Rate Limiting

| File | Description |
|------|-------------|
| [`ratelimit_token_bucket.yaml`](ratelimit_token_bucket.yaml) | Token bucket rate limiting algorithm |
| [`ratelimit_fixed_window.yaml`](ratelimit_fixed_window.yaml) | Fixed window rate limiting algorithm |
| [`ip_ban.yaml`](ip_ban.yaml) | Automatically ban IPs exceeding any rule threshold (multiple window/threshold/ban-duration rules), with auto-unban and whitelist |

### Advanced Features

| File | Description |
|------|-------------|
| [`health_check.yaml`](health_check.yaml) | Active/passive health checking with automatic failover |
| [`circuit_breaker.yaml`](circuit_breaker.yaml) | Circuit breaker pattern for fault tolerance |
| [`matchers.yaml`](matchers.yaml) | Advanced request matching (path/host/header/method) |
| [`reverse_proxy.yaml`](reverse_proxy.yaml) | Basic reverse proxy with forward headers |
| [`http_to_grpc.yaml`](http_to_grpc.yaml) | HTTP to gRPC transcoding with proto descriptors |
| [`tcp_proxy.yaml`](tcp_proxy.yaml) | TCP layer proxy with IP filtering |
| [`static_file.yaml`](static_file.yaml) | Static file serving with caching headers |
| [`openapi_convert.yaml`](openapi_convert.yaml) | OpenAPI spec conversion to routing rules |

## Configuration Reference

### Top-level Fields

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `log_level` | string | `info` | Log level: trace/debug/info/warn/error |
| `admin_port` | int | `9999` | Admin API port |
| `health_check_log_enabled` | bool | `false` | Enable health check logging |
| `upstream_timeout_secs` | int | `5000` | Upstream timeout in milliseconds |
| `servers` | array | `[]` | Server configurations |

### Server Configuration

| Field | Type | Description |
|-------|------|-------------|
| `listen` | int | Listening port |
| `protocol` | string | Protocol: http/https/tcp/http2/http2tls |
| `domains` | array | TLS domain names (for https) |
| `routes` | array | Route configurations |

### Route Configuration

| Field | Type | Description |
|-------|------|-------------|
| `route_id` | string | Unique route identifier |
| `matchers` | array | Request matching rules |
| `forward_to` | object/string | Backend destination |
| `middlewares` | array | Middleware chain |
| `health_check` | object | Health check configuration |
| `timeout` | object | Request timeout configuration |

### Forward Types

| Type | Description |
|------|-------------|
| `single` | Single backend with `target` field |
| `weight` | Weight-based routing with `targets` array |
| `poll` | Round-robin routing with `targets` array |
| `random` | Random routing with array of endpoints |
| `header` | Header-based routing |
| `file` | Static file serving |

### Middleware Types

| Kind | Description |
|------|-------------|
| `forward_headers` | Add X-Real-IP and X-Forwarded-For headers |
| `request_headers` | Add/remove custom headers |
| `authentication` | API Key, Basic Auth, or JWT |
| `rate_limit` | Token bucket or fixed window |
| `allow_deny_list` | IP whitelist/blacklist |
| `cors` | CORS configuration |
| `circuit_breaker` | Circuit breaker pattern |
| `rewrite_headers` | Response header modification |

## Testing Examples

Most examples use placeholder backends. For testing:

```bash
# Start a simple test backend
python -m http.server 8090

# Or use httpbin for testing
# Update forward_to to: http://httpbin.org
```

## Additional Resources

- [Main README](../../../README.md)
- [Configuration Documentation](../../../README.md#configuration-reference)
- [Benchmark Results](../../../benchmarks.md)
