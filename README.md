# Spire

<div align="center">

**High-Performance Rust Proxy/Gateway**

[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](./licence)
[![Rust](https://img.shields.io/badge/rust-1.70+-orange.svg)](https://www.rust-lang.org/)
[![GitHub release](https://img.shields.io/github/v/release/lsk569937453/spire)](https://github.com/lsk569937453/spire/releases)

</div>

## Overview

Spire is a high-performance proxy and gateway system written in Rust, designed to deliver efficient and reliable reverse proxy services. Built on Tokio async runtime and Hyper HTTP library, Spire excels in performance benchmarks with low latency and high throughput characteristics.

## Key Features

- **Exceptional Performance** - Built on Rust async runtime with support for high-concurrency connection handling
- **Low Resource Consumption** - Only 4MB startup memory, minimal runtime memory footprint
- **Multi-Protocol Support** - HTTP/1.1, HTTP/2, gRPC, TCP/TLS proxy
- **Intelligent Routing** - Path-based, weight-based, round-robin, random, header-based routing strategies
- **Security Authentication** - API Key, Basic Auth, IP whitelist/blacklist, CORS
- **Traffic Control** - Token bucket rate limiting, circuit breaker protection, automatic IP banning
- **Health Checking** - Active/passive health checks with automatic failover
- **TLS Support** - HTTPS/TLS encrypted connections, ACME auto certificate acquisition
- **Monitoring Metrics** - Prometheus metrics export
- **Dynamic Configuration** - Hot reload support for configuration files

## Feature Details

### Routing Strategies

| Strategy | Description |
|----------|-------------|
| **Weight-based** | Distribute traffic by configured weights, supports canary deployments |
| **Round-robin** | Distribute requests sequentially for load balancing |
| **Random** | Randomly select backend service |
| **Header-based** | Route to different backends based on request headers |
| **Host-based** | Virtual host routing based on Host header |

### Middleware Capabilities

- **Authentication** - API Key, Basic Auth
- **Access Control** - IP whitelist/blacklist
- **Rate Limiting** - Token bucket algorithm with IP-based limiting
- **IP Banning** - Automatically ban IPs sending too many requests, with auto-unban and whitelist
- **Circuit Breaker** - Automatic circuit breaking for failing backends
- **CORS** - Cross-origin resource sharing configuration
- **Header Manipulation** - Add/remove/modify request headers

### Health Checking

- **HTTP Health Check** - Check service status via HTTP endpoints
- **TCP Health Check** - TCP connection verification
- **Active Check** - Periodic active probing of backends
- **Passive Check** - Judgment based on request failures
- **Failover** - Automatic removal of failed nodes, re-add on recovery

### Protocol Support

| Protocol | Description |
|----------|-------------|
| HTTP | HTTP/1.1 proxy |
| HTTPS | TLS-encrypted HTTP proxy |
| TCP | TCP layer proxy |
| HTTP2 | HTTP/2 and gRPC proxy |
| HTTP2TLS | TLS-encrypted HTTP/2 proxy |

### ACME Auto Certificate

Supports automatic certificate acquisition and renewal for Let's Encrypt and ZeroSSL.

## Performance Benchmarks

Docker Compose based performance testing. Test conditions: 100,000 requests, 250 concurrent connections, 4-core CPU, 8GB memory limit.

### Benchmark Results

| Proxy | RPS | Avg Latency | P99 Latency |
|-------|-----|-------------|-------------|
| Nginx 1.23.3 | 148,677 | 1.6ms | 4.9ms |
| **Spire** | **114,512** | **2.2ms** | **11.8ms** |
| HAProxy 2.7.3 | 85,406 | 2.9ms | 41.7ms |
| Traefik 2.9.8 | 59,487 | 4.1ms | 12.2ms |
| Envoy 1.22.8 | 48,040 | 5.1ms | 51.5ms |
| Caddy 2.6.4 | 13,168 | 18.3ms | 106.5ms |

### Resource Consumption

- **Startup Memory**: 4MB
- **Runtime Memory**: ~35MB (under high load)
- **Binary Size**: ~7MB (stripped release build)

For detailed test data, see [benchmarks.md](./benchmarks.md)

## Installation & Deployment

### System Requirements

- Rust 1.70+ or use pre-built binaries
- Linux / macOS / Windows

### Using Pre-built Binaries

Download the binary for your platform from [Releases](https://github.com/lsk569937453/spire/releases):

```bash
# Linux
wget https://github.com/lsk569937453/spire/releases/download/v0.0.24/spire-x86_64-unknown-linux-gnu
chmod +x spire-x86_64-unknown-linux-gnu
./spire-x86_64-unknown-linux-gnu -f config.yaml

# macOS (ARM64)
wget https://github.com/lsk569937453/spire/releases/download/v0.0.24/spire-aarch64-apple-darwin
chmod +x spire-aarch64-apple-darwin
./spire-aarch64-apple-darwin -f config.yaml
```

### Building from Source

```bash
git clone https://github.com/lsk569937453/spire.git
cd spire/rust-proxy
cargo build --release
./target/release/spire -f config.yaml
```

### Docker Deployment

```bash
# Pull pre-built image
docker pull ghcr.io/lsk569937453/spire:latest

# Run container
docker run -d \
  -p 6667:6667 \
  -p 9999:9999 \
  -v $(pwd)/config.yaml:/app/config.yaml \
  ghcr.io/lsk569937453/spire:latest
```

## Quick Start

### Minimal Configuration Example

Create `config.yaml`:

```yaml
log_level: info
admin_port: 9999

servers:
  - listen: 6667
    protocol: http
    routes:
      - route_id: main
        matchers:
          - path:
              value: /
              match_type: prefix
        forward_to:
          kind: single
          target: http://backend-service:8080
```

### Start the Service

```bash
spire -f config.yaml
```

### Advanced Configuration Example

```yaml
log_level: info
admin_port: 9999

servers:
  - listen: 6667
    protocol: https
    domains:
      - example.com
    routes:
      - route_id: api-route
        matchers:
          - path:
              value: /api
              match_type: prefix
          - host:
              value: example.com
        forward_to:
          kind: weight_based
          routes:
            - { endpoint: http://backend1:8080, weight: 70 }
            - { endpoint: http://backend2:8080, weight: 30 }
        health_check:
          kind: http
          path: /health
          interval: 10
          timeout: 5
        middlewares:
          - kind: authentication
            api_key:
              key: X-API-Key
              value: your-secret-key
          - kind: rate_limit
            token_bucket:
              capacity: 100
              rate_per_unit: 100
              unit: second

acme:
  kind: lets_encrypt
```

## Configuration Reference

### Top-level Configuration

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `log_level` | string | - | Log level: trace/debug/info/warn/error |
| `admin_port` | int | 9999 | Admin interface port |
| `health_check_log_enabled` | bool | false | Enable health check logging |
| `upstream_timeout_secs` | int | 5000 | Upstream request timeout (milliseconds) |
| `servers` | array | [] | Server configuration list |
| `acme` | object | lets_encrypt | ACME certificate configuration |

### Server Configuration (servers)

| Parameter | Type | Description |
|-----------|------|-------------|
| `listen` | int | Listening port |
| `protocol` | string | Protocol type: http/https/tcp/http2/http2tls |
| `domains` | array | TLS domain list |
| `routes` | array | Route configuration list |

### Route Configuration (routes)

| Parameter | Type | Description |
|-----------|------|-------------|
| `route_id` | string | Unique route identifier |
| `matchers` | array | Matcher rule list |
| `forward_to` | object | Backend routing configuration |
| `health_check` | object | Health check configuration |
| `middlewares` | array | Middleware list |
| `path_rewrite` | string | Path rewrite rule |
| `timeout` | object | Timeout configuration |

### Forward Types (forward_to.kind)

- `single` - Single backend
- `weight_based` - Weight-based routing
- `poll` - Round-robin routing
- `random` - Random routing
- `header_based` - Header-based routing

### Middleware Types (middlewares.kind)

- `authentication` - Authentication/authorization
- `rate_limit` - Rate limiting
- `cors` - CORS
- `allow_deny_list` - IP access control
- `circuit_breaker` - Circuit breaker
- `ip_ban` - Automatic banning of high-frequency IPs

## Admin API

Spire provides RESTful admin APIs on the configured `admin_port`:

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/health` | GET | Health check |
| `/config` | GET | Get current configuration |
| `/metrics` | GET | Prometheus metrics |
| `/certificates` | GET | Get certificate list |
| `/certificates` | POST | Upload certificate |

## Development

### Running Tests

```bash
cd rust-proxy
cargo test
```

### Running Benchmarks

```bash
cd benchmarks
docker-compose up
```

### Code Structure

```
rust-proxy/src/
├── main.rs                    # Entry point
├── vojo/                      # Data structure definitions
├── proxy/                     # Proxy core logic
│   ├── http1/                # HTTP/1.1 proxy
│   ├── http2/                # HTTP/2 proxy
│   └── tcp/                  # TCP proxy
├── middleware/                # Middleware implementations
├── health_check/              # Health checking
├── control_plane/             # Control plane API
├── configuration_service/     # Configuration management
└── monitor/                   # Monitoring metrics
```

## Contributing

Contributions, issue reports, and suggestions are welcome!

1. Fork this repository
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Commit your changes (`git commit -m 'Add amazing feature'`)
4. Push to the branch (`git push origin feature/amazing-feature`)
5. Create a Pull Request

## License

This project is licensed under the Apache 2.0 License - see the [LICENSE](./licence) file for details.

## Links

- [Issues](https://github.com/lsk569937453/spire/issues)
- [Releases](https://github.com/lsk569937453/spire/releases)
- [Benchmark Report](./benchmarks.md)