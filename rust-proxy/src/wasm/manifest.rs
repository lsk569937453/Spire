//! Plugin packages: `plugin.toml` discovery, validation and the manifest model.
//!
//! Layout on disk (one directory per package, like every plugin ecosystem that
//! works):
//!
//! ```text
//! ~/.spire/plugins/
//! └── io.example.header-annotator/
//!     ├── plugin.toml
//!     └── dist/plugin.wasm
//! ```
//!
//! The `wasm` path is always resolved **inside** the package directory: a
//! manifest may not reach out of its own folder (no `..`, no absolute path, no
//! symlink escape). The same rule the WIT's capability model assumes.

use serde::Deserialize;
use std::path::Path;
use std::path::PathBuf;

use crate::vojo::app_error::AppError;

/// The WIT package this host implements. A component built against a different
/// package is rejected at load time with the reason instead of failing on the
/// first request.
pub const API_PACKAGE: &str = "spire:filter";
pub const API_MAJOR: u32 = 0;
pub const API_MINOR: u32 = 1;

/// Capabilities a manifest may ask for. Anything else is a load-time error: a
/// typo must not silently produce a plugin that runs with fewer powers than its
/// author thinks.
pub const KNOWN_CAPABILITIES: &[&str] =
    &["host.log", "host.metrics", "host.shared-data", "host.clock"];

/// A `plugin.toml`, validated.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct PluginManifest {
    /// `io.example.header-annotator`. Lowercase, dot/slash/dash separated;
    /// unique within a plugins directory.
    pub id: String,
    pub name: String,
    /// semver, informational (the API version is what gates compatibility).
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub authors: Vec<String>,
    /// `spire:filter@0.1.0`.
    #[serde(default = "default_api")]
    pub api: String,
    /// Relative path to the component inside the package.
    pub wasm: String,
    #[serde(default)]
    pub capabilities: Capabilities,
}

fn default_api() -> String {
    format!("{API_PACKAGE}@{API_MAJOR}.{API_MINOR}.0")
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct Capabilities {
    /// Granted unconditionally: the plugin will not activate without them.
    #[serde(default)]
    pub required: Vec<String>,
    /// Granted when the operator allows them; the plugin must degrade when a
    /// host call answers `denied`.
    #[serde(default)]
    pub optional: Vec<String>,
}

impl Capabilities {
    /// The grants the host actually installs. `optional` is a manifest-side
    /// hint; without an operator policy to consult we grant exactly `required`
    /// plus the safe subset (logging, clock, metrics), and leave
    /// `host.shared-data` to `required` because it mutates process-wide state.
    pub fn granted(&self) -> Vec<String> {
        let mut granted = self.required.clone();
        for cap in &self.optional {
            if matches!(cap.as_str(), "host.log" | "host.clock" | "host.metrics")
                && !granted.contains(cap)
            {
                granted.push(cap.clone());
            }
        }
        granted
    }
}

/// A manifest that was found on disk and passed validation.
#[derive(Debug, Clone)]
pub struct PluginPackage {
    pub manifest: PluginManifest,
    pub wasm_path: PathBuf,
}

impl PluginPackage {
    pub fn load(dir: &Path) -> Result<Self, AppError> {
        let manifest_path = dir.join("plugin.toml");
        let text = std::fs::read_to_string(&manifest_path)
            .map_err(|e| AppError(format!("plugin manifest {}: {e}", manifest_path.display())))?;
        let manifest: PluginManifest = toml::from_str(&text).map_err(|e| {
            AppError(format!(
                "plugin manifest {} is not valid TOML: {e}",
                manifest_path.display()
            ))
        })?;
        manifest.validate(&manifest_path)?;

        let wasm_path = dir.join(&manifest.wasm);
        // `..` was rejected in `validate`, and `Path::join` never lets an
        // absolute `wasm` replace the base, but canonicalise anyway so a
        // symlinked subdirectory cannot point outside the package either.
        let canonical_dir = dir
            .canonicalize()
            .map_err(|e| AppError(format!("plugin directory {}: {e}", dir.display())))?;
        let canonical_wasm = wasm_path.canonicalize().map_err(|e| {
            AppError(format!(
                "plugin {} declares wasm = \"{}\", which cannot be opened: {e}",
                manifest.id, manifest.wasm
            ))
        })?;
        if !canonical_wasm.starts_with(&canonical_dir) {
            return Err(AppError(format!(
                "plugin {}: wasm path {} escapes the package directory",
                manifest.id,
                canonical_wasm.display()
            )));
        }
        if !canonical_wasm.is_file() {
            return Err(AppError(format!(
                "plugin {}: wasm path {} is not a file",
                manifest.id,
                canonical_wasm.display()
            )));
        }

        Ok(Self {
            manifest,
            wasm_path: canonical_wasm,
        })
    }
}

impl PluginManifest {
    fn validate(&self, origin: &Path) -> Result<(), AppError> {
        let bad = |msg: String| {
            Err(AppError(format!(
                "plugin manifest {}: {msg}",
                origin.display()
            )))
        };

        if !valid_id(&self.id) {
            return bad(format!("id `{}` must match [a-z0-9][a-z0-9._-]*", self.id));
        }
        if self.name.trim().is_empty() {
            return bad("name must not be empty".to_string());
        }
        if !valid_semver(&self.version) {
            return bad(format!("version `{}` is not semver", self.version));
        }
        if !self.api_compatible() {
            return bad(format!(
                "api `{}` is not compatible with this host, which implements {API_PACKAGE}@{API_MAJOR}.{API_MINOR}.x",
                self.api
            ));
        }
        if self.wasm.trim().is_empty() {
            return bad("wasm must name the component file".to_string());
        }
        let wasm = Path::new(&self.wasm);
        if wasm.is_absolute()
            || wasm.components().any(|c| {
                matches!(
                    c,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
        {
            return bad(format!(
                "wasm `{}` must be a relative path inside the package",
                self.wasm
            ));
        }
        for cap in self
            .capabilities
            .required
            .iter()
            .chain(self.capabilities.optional.iter())
        {
            if !KNOWN_CAPABILITIES.contains(&cap.as_str()) {
                return bad(format!(
                    "unknown capability `{cap}` (known: {})",
                    KNOWN_CAPABILITIES.join(", ")
                ));
            }
        }
        Ok(())
    }

    /// Same rule the host applies at instantiation: the package and the
    /// major.minor must both match. `0.x` is treated like `1.x` here (minor
    /// bumps may break) because the API is not released yet.
    pub fn api_compatible(&self) -> bool {
        let Some((package, version)) = self.api.split_once('@') else {
            return false;
        };
        if package != API_PACKAGE {
            return false;
        }
        let mut parts = version.split('.');
        let (Some(major), Some(minor)) = (parts.next(), parts.next()) else {
            return false;
        };
        let (Ok(major), Ok(minor)) = (major.parse::<u32>(), minor.parse::<u32>()) else {
            return false;
        };
        major == API_MAJOR && minor <= API_MINOR
    }
}

fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

fn valid_semver(version: &str) -> bool {
    let core = version.split(['-', '+']).next().unwrap_or_default();
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(yaml_like: &str) -> Result<PluginManifest, toml::de::Error> {
        toml::from_str(yaml_like)
    }

    #[test]
    fn accepts_a_minimal_manifest() {
        let m = manifest(
            r#"
id = "io.example.header-annotator"
name = "Header Annotator"
version = "0.1.0"
wasm = "dist/plugin.wasm"
"#,
        )
        .unwrap();
        assert!(m.api_compatible());
        assert!(m.validate(Path::new("plugin.toml")).is_ok());
    }

    #[test]
    fn rejects_a_mismatched_api() {
        let m = manifest(
            r#"
id = "io.example.x"
name = "X"
version = "0.1.0"
api = "spire:filter@1.0.0"
wasm = "dist/plugin.wasm"
"#,
        )
        .unwrap();
        assert!(!m.api_compatible());
        assert!(m.validate(Path::new("plugin.toml")).is_err());
    }

    #[test]
    fn rejects_escaping_and_unknown_capability() {
        let mut m = manifest(
            r#"
id = "io.example.x"
name = "X"
version = "0.1.0"
wasm = "../evil.wasm"
"#,
        )
        .unwrap();
        assert!(m.validate(Path::new("plugin.toml")).is_err());

        m.wasm = "dist/plugin.wasm".to_string();
        m.capabilities.required = vec!["host.root".to_string()];
        assert!(m.validate(Path::new("plugin.toml")).is_err());
    }

    #[test]
    fn optional_capabilities_are_only_granted_when_harmless() {
        let caps = Capabilities {
            required: vec!["host.log".to_string()],
            optional: vec!["host.metrics".to_string(), "host.shared-data".to_string()],
        };
        let granted = caps.granted();
        assert!(granted.contains(&"host.log".to_string()));
        assert!(granted.contains(&"host.metrics".to_string()));
        assert!(!granted.contains(&"host.shared-data".to_string()));
    }
}
