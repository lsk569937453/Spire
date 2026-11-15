# header-annotator — a reference Spire WASM plugin

A deliberately small plugin that exercises **every** part of the ABI, so it can be
read as documentation and copied as a starting point.

What it does:

| Hook | Behaviour | Demonstrates |
|---|---|---|
| `lifecycle.start` | logs the API version and granted capabilities | `host.log` |
| `lifecycle.configure` | parses the route's `config:` blob **once** | config is not re-parsed per request |
| `request_filter.on-request-headers` | adds `x-served-by: <value>`; bumps a shared counter | `host.shared-data`, header mutation |
| `response_filter.on-response-headers` | adds `x-spire-upstream-status` | reading the upstream response |
| `response_filter.on-log` | counts exchanges per status, logs the summary | `host.metrics`, `host.clock` |

## Build

```sh
rustup target add wasm32-wasip2
cargo build --release --target wasm32-wasip2
mkdir -p dist
cp target/wasm32-wasip2/release/spire_plugin_header_annotator.wasm dist/plugin.wasm
```

`dist/plugin.wasm` is the component the gateway loads. It is committed so that
`cargo test --features wasm` in the gateway repository runs the real end-to-end
test on a machine without the `wasm32-wasip2` target; the per-plugin `target/`
directory is not.

## Install

```sh
mkdir -p ~/.spire/plugins/io.example.header-annotator/dist
cp plugin.toml ~/.spire/plugins/io.example.header-annotator/
cp dist/plugin.wasm ~/.spire/plugins/io.example.header-annotator/dist/
```

Then reference it from a route (see `../../../config/examples/wasm_plugin.yaml`):

```yaml
middlewares:
  - kind: wasm
    plugin: io.example.header-annotator
    config: |
      header=x-served-by
      value=edge-a
```

## Rules the gateway enforces

1. **The WIT copy in `wit/` must match the host's `rust-proxy/wit/spire-filter.wit`
   byte for byte.** A test in the gateway (`wasm::tests::plugin_wit_matches_host_wit`)
   fails when they drift. Copy, never edit.
2. `plugin.toml`'s `api` must be `spire:filter@0.1.x`; the `wasm` path must stay
   inside the package directory.
3. A capability you do not declare is a capability you cannot use: the matching
   host call returns `denied(...)`, it does not trap. Degrade, do not crash.
4. Configuration lives in the gateway's route, not here. `configure()` is called
   once per distinct blob, not per request.
5. Budgets (`timeout_ms`, `fuel`, `max_memory_bytes`) belong to the **operator**
   configuring the route. This manifest cannot raise them.
