# Spire

<div align="center">

**High-Performance Rust Proxy/Gateway**

[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](./licence)
[![Rust](https://img.shields.io/badge/rust-1.85+-orange.svg)](https://www.rust-lang.org/)
[![GitHub release](https://img.shields.io/github/v/release/lsk569937453/spire)](https://github.com/lsk569937453/spire/releases)

</div>

## Overview

Spire is a high-performance proxy and gateway system written in Rust, designed to deliver efficient and reliable reverse proxy services. Built on Tokio async runtime and Hyper HTTP library, Spire excels in performance benchmarks with low latency and high throughput characteristics.

## Key Features

- **Exceptional Performance** - Built on Rust async runtime with support for high-concurrency connection handling
- **Low Resource Consumption** - Only 4MB startup memory, minimal runtime memory footprint
- **Multi-Protocol Support** - HTTP/1.1, HTTP/2, gRPC, TCP/TLS proxy
- **Intelligent Routing** - Path-based, weight-based, round-robin, random, header-based routing strategies
- **Security Authentication** - API Key, Basic Auth, JWT, IP whitelist/blacklist, CORS
- **Traffic Control** - Token bucket / fixed window rate limiting per client, circuit breaker protection, automatic IP banning
- **Health Checking** - Active/passive health checks with automatic failover
- **TLS Support** - HTTPS/TLS encrypted connections, ACME auto certificate acquisition
- **Monitoring Metrics** - Prometheus metrics export
- **Dynamic Configuration** - Hot reload on `SIGHUP` (nginx-style); no HTTP admin API
- **WebAssembly Plugins** - Per-route L7 logic in a sandboxed WebAssembly component (optional `wasm` feature); see [docs/plugin-system.md](./docs/plugin-system.md)

## Feature Details

### Routing Strategies

| Strategy | Description |
|----------|-------------|
| **Weight-based** | Distribute traffic by configured weights, supports canary deployments |
| **Round-robin** | Distribute requests sequentially for load balancing |
| **Random** | Randomly select backend service |
| **Header-based** | Route to different backends based on request headers |
| **Static file** | Serve a directory from disk |

### Middleware Capabilities

- **Authentication** - Basic Auth, API Key, JWT (HS256/HS384/HS512)
- **Access Control** - IP whitelist/blacklist, `X-Forwarded-For` aware when `trusted_proxies` is configured
- **Rate Limiting** - Token bucket or fixed window, keyed per client IP / header value / CIDR
- **IP Banning** - Automatically ban IPs sending too many requests, with auto-unban and whitelist
- **Circuit Breaker** - Automatic circuit breaking for failing backends
- **CORS** - Cross-origin resource sharing, with standards-compliant preflight handling
- **Header Manipulation** - Add/remove/modify request and response headers
- **Compression** - gzip / zstd response compression

### Health Checking

- **HTTP Health Check** - Check service status via HTTP endpoints
- **Active Check** - Periodic active probing of backends
- **Failover** - Automatic removal of failed nodes, re-add on recovery

### Protocol Support

| `protocol` | Description |
|------------|-------------|
| `http` | HTTP/1.1 proxy |
| `https` | TLS-encrypted HTTP/1.1 proxy (per-domain SNI certificates) |
| `tcp` | TCP layer proxy |
| `http2` | HTTP/2 (h2c) and gRPC proxying, cleartext |
| `http2tls` | HTTP/2 and gRPC proxying over TLS |

`protocol: tls` is still accepted as an alias for `http2`.

### ACME Auto Certificate

Supports automatic certificate acquisition and renewal for Let's Encrypt and ZeroSSL.
Certificates are stored under `~/.spire/domains/<domain>/{cert,key}.pem`.

## Security Model

Read this before exposing Spire to the internet.

- **The only listening management surface is the read-only observability listener**
  (`GET /health`, `GET /metrics`). It is bound to `127.0.0.1:8870` by default and needs no token
  because it cannot read or rewrite the configuration; there is no HTTP admin API at all.
  Expose it deliberately (`observability.listen`) when Prometheus scrapes from another host.
- There is **no way to rewrite the configuration over the network**. The configuration file is the
  single source of truth; apply changes with `spire reload --pid <pid>` (sends `SIGHUP`).
- Configuration, including TLS keys, API keys and JWT secrets, is **never printed** at the default
  log levels, and secret fields are redacted in `Debug` output.
- `X-Forwarded-For` / `X-Real-IP` are only trusted when the immediate peer is listed in
  `trusted_proxies`; otherwise a client could spoof its address and bypass IP based rate limiting,
  banning or allow lists.
- Proxy access logs are emitted at `debug` level (one formatted line per request is measurable at
  high request rates). Set `log_level: debug` to enable them.
- `jwt` authentication is compiled with the `rust_crypto` backend; without it `jsonwebtoken` panics
  at runtime instead of validating tokens.

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

Note: the benchmark numbers above were measured with the historical
[`benchmarks/spires/app_config.yaml`](./benchmarks/spires/app_config.yaml) configuration. Re-run
`cd benchmarks && docker compose up` to reproduce them on your hardware.

## Installation & Deployment

### System Requirements

- Rust **1.85+** (the crate uses `edition = "2024"`) or use pre-built binaries
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

# Run container (the observability listener stays inside the container network)
docker run -d \
  -p 6667:6667 \
  -v $(pwd)/config.yaml:/etc/spire/config.yaml \
  ghcr.io/lsk569937453/spire:latest
```

The bundled [`docker/spires/app_config.yaml`](./docker/spires/app_config.yaml) is a working
configuration for the image; adjust the upstream and mount your own file for real deployments.

## Quick Start

### Minimal Configuration Example

Create `config.yaml`:

```yaml
log_level: info
servers:
  - listen: 6667
    protocol: http
    routes:
      - matchers:
          - kind: path
            value: /
            match_type: prefix
        # A plain string is the single-backend shorthand.
        forward_to: http://backend-service:8080
```

### Start the Service

```bash
spire -f config.yaml
```

### Advanced Configuration Example

This example is the same shape as
[`config/examples/middle_wares.yaml`](./rust-proxy/config/examples/middle_wares.yaml), which is
validated in CI:

```yaml
log_level: info
# Read-only observability listener: /health and /metrics, no authentication.
observability:
  listen: 127.0.0.1:8870
# Trust the load balancer in front of Spire for X-Forwarded-For handling.
trusted_proxies:
  - 10.0.0.0/8

servers:
  - listen: 6667
    protocol: http
    routes:
      - route_id: api-route
        matchers:
          - kind: path
            value: /api
            match_type: prefix
          - kind: method
            values: [GET, POST]
        # Weighted (canary) routing to two backends.
        forward_to:
          kind: weight
          targets:
            - endpoint: http://backend1:8080
              weight: 70
            - endpoint: http://backend2:8080
              weight: 30
        health_check:
          kind: http_get
          base_health_check_param:
            timeout: 5
            interval: 10
          path: /health
        liveness_config:
          min_liveness_count: 1
        middlewares:
          - kind: authentication
            scheme: api_key
            key: X-API-Key
            value: your-secret-key
          - kind: rate_limit
            limiter: token_bucket
            capacity: 100
            rate_per_unit: 100
            unit:
              kind: Second
            scope:
              kind: IP
          - kind: cors
            allowed_origins: "*"
            allowed_methods: [GET, POST]
            allowed_headers: "*"

acme:
  kind: lets_encrypt
```

## Configuration Reference

### Top-level Configuration

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `log_level` | string | `off` | Log level: trace/debug/info/warn/error |
| `observability` | object | `listen: 127.0.0.1:8870`, `enabled: true` | Read-only listener serving `GET /health` and `GET /metrics`. No authentication; `enabled: false` runs without it |
| `trusted_proxies` | array | `[]` | CIDR list of proxies whose `X-Forwarded-For` is trusted |
| `health_check_log_enabled` | bool | false | Enable health check logging |
| `upstream_timeout_ms` | int | `5000` | Upstream request timeout in milliseconds (alias of the deprecated `upstream_timeout_secs`) |
| `upstream_concurrency_limit` | int | *auto-derived* | Global fallback for the per-endpoint concurrency limit. Absent = derived at startup from the container memory limit (cgroup): `global permits = mem × 50% ÷ 64 KiB`, endpoint default = `min(10000, global ÷ 4)`; the global budget is a process-wide OOM guard. Explicit value wins |
| `servers` | array | [] | Server configuration list |
| `acme` | object | `lets_encrypt` | ACME certificate configuration |

### Server Configuration (servers)

| Parameter | Type | Description |
|-----------|------|-------------|
| `listen` | int | Listening port |
| `protocol` | string | `http` / `https` / `tcp` / `http2` / `http2tls` |
| `domains` | array | TLS domain list (required for `https` and `http2tls`) |
| `routes` | array | Route configuration list |

### Route Configuration (routes)

| Parameter | Type | Description |
|-----------|------|-------------|
| `route_id` | string | Unique route identifier (also used as a metrics label) |
| `matchers` | array | Matcher rule list |
| `forward_to` | string \| object | Backend routing configuration |
| `health_check` | object | Health check configuration |
| `liveness_config` | object | `min_liveness_count`, required for health checking |
| `middlewares` | array | Middleware list |
| `path_rewrite` | string | Path rewrite rule |
| `timeout` | object | `request_timeout` in milliseconds (headers phase and per-body-frame idle timeout; exceeded upstreams answer 504) |
| `concurrency` | object | Per-endpoint backpressure: `limit` (max in-flight), `queue_length` (max waiters, `0` = fail fast), `queue_timeout_ms` (max wait). Exhausted limit answers 503 + `Retry-After` |

### Matchers (matchers)

| Kind | Fields | Notes |
|------|--------|-------|
| `path` | `value`, `match_type` (`prefix`/`exact`/`regex`) | Prefix is the default |
| `host` | `value` | Anchored regex: `example.com` will not match `example.com.evil.tld` |
| `header` | `name`, `value` | Anchored regex match on the header value |
| `method` | `values` | List of HTTP methods |

### Forward Types (forward_to.kind)

- `poll` - Round-robin routing (`targets`)
- `random` - Random routing (`targets`)
- `weight` - Weight-based routing (`targets` with `endpoint` + `weight`)
- `header` - Header-based routing (`targets` with `endpoint`, `header`, `match`)
- `file` - Static file serving (`doc_root`)

A bare string (`forward_to: http://backend:8080`) is shorthand for a single backend.

### Middleware Types (middlewares.kind)

| `kind` | Key fields |
|--------|-----------|
| `authentication` | `scheme`: `basic` (`credentials`) / `api_key` (`key`, `value`) / `jwt` (`secret`, `algorithm`, `issuer`, `audience`) |
| `rate_limit` | `limiter`: `token_bucket` (`capacity`, `rate_per_unit`, `unit`, `scope`) / `fixed_window` (`rate_per_unit`, `unit`, `scope`) |
| `cors` | `allowed_origins`, `allowed_methods`, `allowed_headers`, `allow_credentials`, `max_age` |
| `allow_deny_list` | `rules` (`policy`: `allow_all`/`deny_all`/`allow`/`deny`, `value`) |
| `circuit_breaker` | `failure_threshold`, `consecutive_failures`, `cooldown`, `half_open_requests` |
| `ip_ban` | `rules` (`threshold`, `window`, `ban_duration`), `whitelist` |
| `rewrite_headers` | Response header rewrites |
| `forward_headers` | Adds `X-Real-IP` / `X-Forwarded-For` |
| `request_headers` | Request header rewrites |
| `compression` | `compression_type` (`gzip`/`zstd`/`both`), `level`, `min_size` |

`scope` accepts `kind: IP` (per client IP — the `value` field is accepted but ignored),
`kind: Header` (`key`, optional `value`) or `kind: Iprange` (`value` as a CIDR, one shared bucket).

## Observability and Reload

Spire has **no HTTP admin API**: the configuration file is the only way in, and it can only be
changed on disk. The only listener besides the proxy ports is the read-only observability one:

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/health` | GET | Liveness probe (`ok`) |
| `/metrics` | GET | Prometheus metrics |

It is unauthenticated because it cannot read or write the configuration; it binds to
`127.0.0.1:8870` by default. Set `observability.listen` explicitly when Prometheus scrapes from
another host, and put it behind a firewall — the endpoints are still readable by anyone who can
reach them.

### Reloading

Configuration reloads are triggered by `SIGHUP` (the nginx model):

```bash
spire reload --pid $(pidof spire)     # sends SIGHUP
```

or, without the CLI:

```bash
# systemd
ExecReload=/bin/kill -HUP $MAINPID

# docker
docker kill -s HUP <container>
```

On reload Spire re-reads the file it was started with:

- A file that fails to parse is reported in the log and **ignored**; the previous configuration
  keeps serving (reloads are never fatal).
- Listeners may be **added and removed**; new ports are bound before anything is committed, so a
  port that cannot be bound aborts the whole reload and leaves the running configuration intact.
- Changing the `protocol` of a port that is already listening requires a restart; so does changing
  the `domains` of an existing TLS listener (its SNI resolver is built once).
- `observability.listen` and `log_level` require a restart (the running values are kept and the
  change is logged).
- `SIGHUP` does not exist on Windows: restart the process there.

### Inspecting the configuration

```bash
spire validate -c config.yaml        # like `nginx -t`
spire config expand -c config.yaml   # like `nginx -T`: parsed configuration, defaults applied
```

### Metrics

Metrics use `route` (the matched `route_id`) as label, never the raw request path — a path label
would create one time series per URL and grow memory without bound. Keep `route_id` values stable
and low-cardinality.

## Development

### Running Tests

```bash
cd rust-proxy
cargo test
```

### Linting

```bash
cd rust-proxy
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
```

CI (`.github/workflows/ci.yaml`) runs formatting, clippy, the full test suite on Linux and Windows,
and validates every example configuration.

### Running Benchmarks

```bash
cd benchmarks
docker compose up
```

### Code Structure

```
rust-proxy/src/
├── main.rs                    # Entry point
├── vojo/                      # Data structure definitions
├── proxy/                     # Proxy core logic
│   ├── http1/                 # HTTP/1.1 proxy
│   ├── http2/                 # HTTP/2 proxy
│   └── tcp/                   # TCP proxy
├── middleware/                # Middleware implementations
├── health_check/              # Health checking
├── observability/             # Read-only /health + /metrics listener
├── tls/                       # TLS certificates and ACME renewal
├── configuration_service/     # Configuration loading, reload, logging
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

This project is licensed under the Apache 2.0 License - see the [licence](./licence) file for details.

## Links

- [Issues](https://github.com/lsk569937453/spire/issues)
- [Releases](https://github.com/lsk569937453/spire/releases)
- [Benchmark Report](./benchmarks.md)
