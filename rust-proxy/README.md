# spire-proxy

Spire is a high-performance proxy and gateway written in Rust (HTTP/1.1, HTTP/2, gRPC, TCP/TLS)
with routing, authentication, rate limiting, health checking, TLS/ACME and a Prometheus exporter.

This crate builds the `spire` binary. The full documentation, benchmarks and the configuration
reference live in the repository root:

- [README](https://github.com/lsk569937453/Spire#readme) — features, configuration reference, observability & reload
- [Benchmarks](https://github.com/lsk569937453/Spire/blob/main/benchmarks.md)
- [Example configurations](./config/examples) — every file is validated in CI

## Usage

```bash
cargo build --release
./target/release/spire -f config.yaml
```

```yaml
log_level: info
observability:
  listen: 127.0.0.1:8870    # read-only /health + /metrics, bound to loopback by default
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
spire validate -c config.yaml      # check a configuration file (like nginx -t)
spire config expand -c config.yaml # print the expanded configuration (like nginx -T)
spire examples --list              # list the bundled examples
spire reload --pid 1234            # send SIGHUP: re-read the config file from disk
```

> There is no HTTP admin API. The configuration file is the only source of truth; the only
> listener besides the proxy ports is the unauthenticated, read-only observability one, which
> binds to loopback unless `observability.listen` says otherwise.

## Development

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
```

## License

Apache-2.0 — see [licence](https://github.com/lsk569937453/Spire/blob/main/licence).
