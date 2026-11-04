# spire-proxy

Spire is a high-performance proxy and gateway written in Rust (HTTP/1.1, HTTP/2, gRPC, TCP/TLS)
with routing, authentication, rate limiting, health checking, TLS/ACME and a Prometheus exporter.

This crate builds the `spire` binary. The full documentation, benchmarks and the configuration
reference live in the repository root:

- [README](https://github.com/lsk569937453/Spire#readme) — features, configuration reference, admin API
- [Benchmarks](https://github.com/lsk569937453/Spire/blob/main/benchmarks.md)
- [Example configurations](./config/examples) — every file is validated in CI

## Usage

```bash
cargo build --release
./target/release/spire -f config.yaml
```

```yaml
log_level: info
admin_port: 8870            # control plane, bound to 127.0.0.1 by default
servers:
  - listen: 6667
    protocol: http
    routes:
      - matchers:
          - kind: path
            value: /
            match_type: prefix
        forward_to: http://backend-service:8080
```

```bash
spire validate -c config.yaml     # check a configuration file
spire examples --list             # list the bundled examples
spire query --port 8870           # read the running configuration from the control plane
```

> The control plane can rewrite the whole configuration. It listens on loopback unless
> `admin_listen` is set, and non-loopback bindings should always be protected with `admin_token`.

## Development

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
```

## License

Apache-2.0 — see [licence](https://github.com/lsk569937453/Spire/blob/main/licence).
