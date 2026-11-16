# ratelimit-external

Spire WASM plugin (`spire:filter@0.1.0`) that asks an external HTTP limiter for
a decision on every request.

## Configuration

`config:` is a `key=value` blob, parsed once in `configure`:

```yaml
- kind: wasm
  plugin: io.example.ratelimit-external
  config: |
    url=https://limiter.internal/check
    key=ip
    on_error=closed
  fail: closed
  timeout_ms: 50
  fuel: 5000000
  http_allowed_hosts: ["limiter.internal"]
  http_timeout_ms: 500
  max_http_body_bytes: 8192
```

| key | default | meaning |
| --- | --- | --- |
| `url` | — (required) | absolute `http(s)` URL of the limiter endpoint |
| `key` | `ip` | `ip` (resolved client IP), `header`, or `route` (`route_id`) |
| `header` | — | request header used as the key; required when `key=header` |
| `on_error` | `closed` | `closed` → local 503, `open` → forward with a warning |

## Behaviour

The plugin POSTs `{"key":…,"route_id":…,"method":…,"path":…}` with
`content-type: application/json` and expects
`{"allowed": true|false, "retry_after": <seconds>}`.

* `2xx` + `allowed: true` → forward unchanged.
* `2xx` + `allowed: false` → local `429`, `content-type: application/json`,
  `retry-after` from `retry_after` (default 1), body
  `{"error":"rate limited"}`.
* non-2xx, unparseable body, or a `http-call` error → per `on_error`:
  `503` + `{"error":"rate limiter unavailable"}`, or forward plus a warning.

`key=header` with the header absent sends an empty key; the limiter still
answers, so an unidentifiable client is not silently allowed through.

## Metric

`spire_plugin_io_example_ratelimit_external_requests_total{plugin="…",outcome="…"}`
with `outcome` one of `allow`, `deny`, `error`.

## Capabilities

`host.http-call`, `host.log`, `host.metrics`.

## Build

```sh
rustup target add wasm32-wasip2
cargo build --release --target wasm32-wasip2
copy target\wasm32-wasip2\release\spire_plugin_ratelimit_external.wasm dist\plugin.wasm
```
