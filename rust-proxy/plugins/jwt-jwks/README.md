# jwt-jwks

Spire WASM plugin (`spire:filter@0.1.0`) that validates a `Bearer` JWT against
a JWKS document fetched over HTTP and cached in shared data.

## Configuration

`config:` is a `key=value` blob, parsed once in `configure`:

```yaml
- kind: wasm
  plugin: io.example.jwt-jwks
  config: |
    jwks_url=https://idp.internal/.well-known/jwks.json
    issuer=https://issuer.example
    audience=spire-api
    cache_seconds=300
    header=authorization
    strip=false
  fail: closed
  timeout_ms: 80
  fuel: 8000000
  http_allowed_hosts: ["idp.internal"]
  http_timeout_ms: 1500
  max_http_body_bytes: 262144
```

| key | default | meaning |
| --- | --- | --- |
| `jwks_url` | — (required) | absolute `http(s)` URL of the JWKS document |
| `issuer` | — | expected `iss`; becomes a required claim when set |
| `audience` | — | expected `aud`; becomes a required claim when set |
| `alg` | `RS256` | pinned algorithm (RSA family: `RS*`/`PS*`) |
| `cache_seconds` | `300` | JWKS cache lifetime, capped at 3600 |
| `header` | `authorization` | header carrying `Bearer <token>` |
| `strip` | `false` | remove that header before forwarding |

## Behaviour

1. Read `Bearer <token>` from the configured header.
2. Decode the JOSE header (base64url, unpadded) for `kid`/`alg`; a mismatch with
   the configured `alg` is rejected before any key lookup.
3. Use the cached JWKS (`shared-data` namespace `jwks`, key `jwks_url`, value =
   8-byte little-endian expiry millis + JSON) while it is fresh; otherwise
   `GET` the URL, cache only on `2xx`, and warn + reject on non-2xx/error.
4. Select the RSA key with a matching `kid` (`kty=RSA`, `use` absent or `sig`)
   and verify with `DecodingKey::from_rsa_components(n, e)` plus
   `Validation::new(alg)` and the configured issuer/audience.
5. Valid token → forward (optionally without the credential header). Every
   failure → local `401` with `www-authenticate: Bearer` and
   `{"error":"invalid token"}`.

## Metric

`spire_plugin_io_example_jwt_jwks_token_checks_total{plugin="…",outcome="…"}`
with `outcome` one of `ok`, `invalid`, `error`.

## Capabilities

`host.http-call`, `host.shared-data`, `host.clock`, `host.log`, `host.metrics`.

## Build

```sh
rustup target add wasm32-wasip2
cargo build --release --target wasm32-wasip2
copy target\wasm32-wasip2\release\spire_plugin_jwt_jwks.wasm dist\plugin.wasm
```
