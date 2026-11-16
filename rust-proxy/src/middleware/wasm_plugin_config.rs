//! The configuration surface of a WASM data-plane plugin, exactly as it appears
//! in a route's `middlewares:` list.
//!
//! This module is compiled **unconditionally** (no `wasmtime`, no `toml`): it
//! only describes what the operator asked for. Everything that actually runs a
//! component lives behind the `wasm` feature in [`crate::wasm`]. Keeping the
//! two apart is what lets the default build stay dependency-free while a
//! configuration that mentions `kind: wasm` is still parsed, validated and
//! *rejected with a clear message* instead of being silently ignored.

use serde::Deserialize;
use serde::Serialize;

/// What happens to the request when a plugin fails (trap, fuel exhaustion,
/// epoch timeout, poisoned instance, missing artifact).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FailPolicy {
    /// Log, count, and carry on as if the plugin had returned
    /// `proceed([])`. The default: a broken observability/annotation plugin
    /// must not take the gateway down.
    #[default]
    Open,
    /// Answer `503 Service Unavailable` without calling the upstream. Choose
    /// this for plugins the request's safety depends on (authz, WAF).
    Closed,
}

/// How much of the body the plugin is allowed to see.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BodyMode {
    /// Headers and trailers only; bodies are never copied through the sandbox.
    /// The default, and the only mode that keeps a proxy a proxy: it costs no
    /// extra memory and never delays the first byte.
    #[default]
    HeadersOnly,
    /// Buffer the whole body (bounded by `max_body_bytes`) and invoke the body
    /// callbacks once with `end-of-stream` set.
    Buffered,
    /// Invoke the body callbacks once per chunk as frames arrive.
    Stream,
}

fn default_timeout_ms() -> u32 {
    5
}
fn default_fuel() -> u64 {
    2_000_000
}
fn default_max_memory_bytes() -> u64 {
    16 * 1024 * 1024
}
fn default_max_body_bytes() -> u64 {
    1024 * 1024
}
fn default_max_output_bytes() -> u64 {
    64 * 1024
}
fn default_http_timeout_ms() -> u32 {
    2_000
}
fn default_max_http_body_bytes() -> u64 {
    256 * 1024
}

/// One `kind: wasm` entry of `route.middlewares`.
///
/// ```yaml
/// middlewares:
///   - kind: wasm
///     plugin: io.example.header-annotator   # manifest id, or a path
///     config: '{"header":"x-served-by"}'    # opaque to the gateway
///     fail: open
///     body: headers_only
///     timeout_ms: 5
/// ```
///
/// Every limit here is *operator* controlled on purpose: a plugin manifest may
/// declare defaults, but it must never be able to raise the budget it runs
/// with. The host takes the intersection of both.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WasmPluginConfig {
    /// Plugin id as declared in its `plugin.toml` (or, for development, a path
    /// to a `.wasm` next to its manifest).
    pub plugin: String,

    /// Opaque blob handed to `lifecycle.configure`. JSON is the convention, not
    /// a requirement: the gateway never parses it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<String>,

    #[serde(default)]
    pub fail: FailPolicy,

    #[serde(default)]
    pub body: BodyMode,

    /// Wall-clock budget for **one** guest callback. Reaching it traps the
    /// instance (which is then discarded) — it is not a soft hint.
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u32,

    /// Abstract CPU budget for one callback. Fuel is deterministic where
    /// `timeout_ms` is not, so both are enforced: fuel catches a hot loop on a
    /// loaded machine, the epoch deadline catches a sleeping/blocking guest.
    #[serde(default = "default_fuel")]
    pub fuel: u64,

    /// Linear-memory ceiling for one instance.
    #[serde(default = "default_max_memory_bytes")]
    pub max_memory_bytes: u64,

    /// Ceiling for `body: buffered` (and for the bytes a body callback may
    /// return in one chunk).
    #[serde(default = "default_max_body_bytes")]
    pub max_body_bytes: u64,

    /// Ceiling for the bytes a single callback may return as header mutations
    /// or a local reply body.
    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes: u64,

    /// Hosts a plugin may reach through `host.http-call`, as `host` or
    /// `host:port`. Empty — the default — refuses every outbound call, so a
    /// plugin cannot open a socket unless the operator lists the destination.
    /// This is the SSRF boundary: a hostname here can resolve anywhere, so only
    /// add names the plugin is meant to talk to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub http_allowed_hosts: Vec<String>,

    /// Wall-clock budget for one `host.http-call`. Separate from `timeout_ms`,
    /// which bounds one guest callback: a network round trip needs more than a
    /// few milliseconds.
    #[serde(default = "default_http_timeout_ms")]
    pub http_timeout_ms: u32,

    /// Ceiling for the body `host.http-call` returns to the guest.
    #[serde(default = "default_max_http_body_bytes")]
    pub max_http_body_bytes: u64,
}

impl WasmPluginConfig {
    /// Rejects values that cannot be enforced or that would let one route
    /// starve the process. Called by `spire validate` and by config load, so a
    /// typo fails at startup rather than in the data path.
    pub fn validate(&self) -> Result<(), String> {
        if self.plugin.trim().is_empty() {
            return Err("wasm middleware: `plugin` must not be empty".to_string());
        }
        // `clamp() != value` rather than `!(clamp() == value)`: the latter
        // parses as a bitwise `!` on the integer and silently accepts 0.
        if self.timeout_ms.clamp(1, 1_000) != self.timeout_ms {
            return Err(format!(
                "wasm middleware: timeout_ms={} is outside 1..=1000",
                self.timeout_ms
            ));
        }
        if self.fuel < 10_000 {
            return Err(format!(
                "wasm middleware: fuel={} is below the 10000 minimum",
                self.fuel
            ));
        }
        const MIB: u64 = 1024 * 1024;
        if !(MIB..=1024 * MIB).contains(&self.max_memory_bytes) {
            return Err(format!(
                "wasm middleware: max_memory_bytes={} is outside 1MiB..=1GiB",
                self.max_memory_bytes
            ));
        }
        if self.max_body_bytes > 64 * MIB {
            return Err(format!(
                "wasm middleware: max_body_bytes={} exceeds 64MiB",
                self.max_body_bytes
            ));
        }
        if self.max_output_bytes > 16 * MIB {
            return Err(format!(
                "wasm middleware: max_output_bytes={} exceeds 16MiB",
                self.max_output_bytes
            ));
        }
        if self.body != BodyMode::HeadersOnly && self.max_body_bytes == 0 {
            return Err(
                "wasm middleware: body buffering is enabled with max_body_bytes=0".to_string(),
            );
        }
        if self.http_timeout_ms.clamp(1, 30_000) != self.http_timeout_ms {
            return Err(format!(
                "wasm middleware: http_timeout_ms={} is outside 1..=30000",
                self.http_timeout_ms
            ));
        }
        if self.max_http_body_bytes > 8 * MIB {
            return Err(format!(
                "wasm middleware: max_http_body_bytes={} exceeds 8MiB",
                self.max_http_body_bytes
            ));
        }
        for host in &self.http_allowed_hosts {
            let valid = !host.is_empty()
                && host.len() <= 260
                && !host.chars().any(|c| c.is_whitespace())
                && !host.contains(['/', '@', '?', '#']);
            if !valid {
                return Err(format!(
                    "wasm middleware: http_allowed_hosts entry `{host}` must be host or host:port"
                ));
            }
        }
        Ok(())
    }

    /// Stable identity of "this plugin, with this configuration".
    ///
    /// Every field that changes what the instance may *do* is part of the key,
    /// not only `plugin`/`config`: two routes that configure the same plugin
    /// with different budgets or a different `http_allowed_hosts` must never
    /// share an instance, or the stricter route would inherit the looser
    /// route's permissions. Only the WASM runtime consumes it; the struct
    /// itself is feature-independent so a build without "wasm" still parses
    /// and rejects `kind: wasm`.
    #[cfg_attr(not(feature = "wasm"), allow(dead_code))]
    pub fn instance_key(&self) -> String {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        use std::hash::Hash as _;
        self.plugin.hash(&mut hasher);
        self.config.as_deref().unwrap_or_default().hash(&mut hasher);
        self.fail.hash(&mut hasher);
        self.body.hash(&mut hasher);
        self.timeout_ms.hash(&mut hasher);
        self.fuel.hash(&mut hasher);
        self.max_memory_bytes.hash(&mut hasher);
        self.max_body_bytes.hash(&mut hasher);
        self.max_output_bytes.hash(&mut hasher);
        self.http_allowed_hosts.hash(&mut hasher);
        self.http_timeout_ms.hash(&mut hasher);
        self.max_http_body_bytes.hash(&mut hasher);
        format!("{:016x}", std::hash::Hasher::finish(&hasher))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let cfg: WasmPluginConfig = serde_yaml::from_str("plugin: io.example.x").unwrap();
        assert_eq!(cfg.fail, FailPolicy::Open);
        assert_eq!(cfg.body, BodyMode::HeadersOnly);
        assert_eq!(cfg.timeout_ms, 5);
        assert!(cfg.http_allowed_hosts.is_empty());
        assert_eq!(cfg.http_timeout_ms, 2000);
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn rejects_bad_http_settings() {
        let mut cfg: WasmPluginConfig = serde_yaml::from_str("plugin: io.example.x").unwrap();
        cfg.http_timeout_ms = 0;
        assert!(cfg.validate().is_err());
        cfg.http_timeout_ms = 2000;
        cfg.max_http_body_bytes = 9 * 1024 * 1024;
        assert!(cfg.validate().is_err());
        cfg.max_http_body_bytes = 1024;
        cfg.http_allowed_hosts = vec!["https://auth.example.com/".to_string()];
        assert!(cfg.validate().is_err());
        cfg.http_allowed_hosts = vec!["auth.example.com:8443".to_string()];
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn instance_key_separates_http_allowlists() {
        let mut a: WasmPluginConfig = serde_yaml::from_str("plugin: p").unwrap();
        let b = a.clone();
        a.http_allowed_hosts = vec!["auth.example.com".to_string()];
        assert_ne!(a.instance_key(), b.instance_key());
    }

    #[test]
    fn rejects_impossible_budgets() {
        let mut cfg: WasmPluginConfig = serde_yaml::from_str("plugin: io.example.x").unwrap();
        cfg.timeout_ms = 0;
        assert!(cfg.validate().is_err());
        cfg.timeout_ms = 5;
        cfg.max_memory_bytes = 1024;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn instance_key_separates_configs() {
        let mut a: WasmPluginConfig = serde_yaml::from_str("plugin: p").unwrap();
        let b = a.clone();
        a.config = Some("{\"x\":1}".to_string());
        assert_eq!(a.instance_key(), a.instance_key());
        assert_ne!(a.instance_key(), b.instance_key());
    }
}
