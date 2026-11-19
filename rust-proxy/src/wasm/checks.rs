//! Route-level configuration checks that need a manifest *and* a route at the
//! same time.
//!
//! Everything here only **warns**. The combinations below are almost always a
//! mistake, but none of them is provably wrong: an operator may genuinely want
//! two auth layers, or a plugin that adds a header the built-in middleware also
//! removes. Refusing to start would be a bigger hammer than the mistake
//! deserves — the budgets the host cannot enforce at all *are* hard errors, and
//! those live in `WasmPluginConfig::validate`.
//!
//! What makes these checks possible at all is `[declares]` in `plugin.toml`
//! (see `manifest::Declares`): the host cannot see which headers a guest writes,
//! so the package has to say so. It is a soft declaration — nothing enforces it
//! at runtime — which is why a missing declaration produces no warning rather
//! than a wrong one.

use super::manifest::PluginKind;
use super::resolve;
use crate::middleware::middlewares::MiddleWares;
use crate::middleware::wasm_plugin_config::FailPolicy;

/// One loaded plugin as it appears in a route, reduced to what the checks need.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Declared {
    pub id: String,
    pub kind: PluginKind,
    pub headers: Vec<String>,
    pub fail_open: bool,
}

/// Everything a route says that the checks care about, extracted once.
fn describe(
    middlewares: &[MiddleWares],
) -> (bool, bool, Vec<String>, Vec<Declared>) {
    let mut has_builtin_auth = false;
    let mut has_cors = false;
    let mut builtin_headers: Vec<String> = Vec::new();

    for middleware in middlewares {
        match middleware {
            MiddleWares::Authentication(_) => has_builtin_auth = true,
            MiddleWares::Cors(_) => has_cors = true,
            MiddleWares::RequestHeaders(rh) => {
                builtin_headers.extend(rh.add.keys().cloned());
                builtin_headers.extend(rh.remove.iter().map(|h| h.to_ascii_lowercase()));
            }
            _ => {}
        }
    }

    let mut plugins = Vec::new();
    for middleware in middlewares {
        let MiddleWares::Wasm(cfg) = middleware else {
            continue;
        };
        if let Ok(plugin) = resolve(&cfg.plugin) {
            plugins.push(Declared {
                id: plugin.manifest.id.clone(),
                kind: plugin.manifest.declares.kind,
                headers: plugin.manifest.declares.headers.clone(),
                fail_open: cfg.fail == FailPolicy::Open,
            });
        }
    }

    (has_builtin_auth, has_cors, builtin_headers, plugins)
}

/// Logs one warning per suspicious combination of `route_id`'s middlewares.
pub fn warn_about_route(route_id: &str, middlewares: &[MiddleWares]) {
    let (has_builtin_auth, has_cors, builtin_headers, plugins) = describe(middlewares);
    let label = if route_id.is_empty() {
        "<unnamed>"
    } else {
        route_id
    };
    for message in conflicts(has_builtin_auth, has_cors, &builtin_headers, &plugins) {
        warn!(route = label, "wasm: {message}");
    }
}

/// The messages, without the logging, so they can be tested without a compiled
/// component.
fn conflicts(
    has_builtin_auth: bool,
    has_cors: bool,
    builtin_headers: &[String],
    plugins: &[Declared],
) -> Vec<String> {
    let mut messages = Vec::new();

    for plugin in plugins {
        if plugin.kind == PluginKind::Auth && plugin.fail_open {
            messages.push(format!(
                "`{}` declares itself an auth plugin but the route sets `fail: open`; a plugin that \
                 traps or cannot be configured will let the request through. Use `fail: closed` for \
                 anything the request's safety depends on",
                plugin.id
            ));
        }
    }

    if has_builtin_auth {
        for plugin in plugins.iter().filter(|p| p.kind == PluginKind::Auth) {
            messages.push(format!(
                "route has the built-in `authentication` middleware and the auth plugin `{}`; the \
                 built-in one runs first and rejects before the plugin sees the request, so the \
                 plugin can never widen access — but two places to debug auth is one too many",
                plugin.id
            ));
        }
    }

    if has_cors {
        for plugin in plugins {
            if let Some(header) = plugin
                .headers
                .iter()
                .find(|header| header.starts_with("access-control-"))
            {
                messages.push(format!(
                    "route has the built-in `cors` middleware and plugin `{}` also writes \
                     `{header}`; plugins run before `cors` on the response, so the built-in \
                     middleware's value is the one the client sees",
                    plugin.id
                ));
            }
        }
    }

    for plugin in plugins {
        for header in &plugin.headers {
            if builtin_headers.iter().any(|h| h == header) {
                messages.push(format!(
                    "`{header}` is written by both the built-in `request_headers` middleware and \
                     plugin `{}`; plugins run after the built-in header middlewares, so the plugin's \
                     value wins",
                    plugin.id
                ));
            }
        }
    }

    messages
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin(id: &str, kind: PluginKind, headers: &[&str], fail_open: bool) -> Declared {
        Declared {
            id: id.to_string(),
            kind,
            headers: headers.iter().map(|h| h.to_string()).collect(),
            fail_open,
        }
    }

    #[test]
    fn an_auth_plugin_that_fails_open_is_reported() {
        let messages = conflicts(
            false,
            false,
            &[],
            &[plugin("io.example.jwt-auth", PluginKind::Auth, &[], true)],
        );
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("fail: open"), "{messages:?}");
    }

    #[test]
    fn an_auth_plugin_that_fails_closed_is_fine() {
        let messages = conflicts(
            false,
            false,
            &[],
            &[plugin("io.example.jwt-auth", PluginKind::Auth, &[], false)],
        );
        assert!(messages.is_empty(), "{messages:?}");
    }

    #[test]
    fn annotate_plugins_never_trigger_the_auth_warning() {
        let messages = conflicts(
            true,
            true,
            &[],
            &[plugin(
                "io.example.security-headers",
                PluginKind::Annotate,
                &["x-frame-options"],
                true,
            )],
        );
        assert!(messages.is_empty(), "{messages:?}");
    }

    #[test]
    fn builtin_auth_plus_auth_plugin_is_reported() {
        let messages = conflicts(
            true,
            false,
            &[],
            &[plugin("io.example.api-key-auth", PluginKind::Auth, &[], false)],
        );
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("authentication"), "{messages:?}");
    }

    #[test]
    fn cors_and_a_plugin_writing_cors_headers_is_reported() {
        let messages = conflicts(
            false,
            true,
            &[],
            &[plugin(
                "io.example.security-headers",
                PluginKind::Annotate,
                &["access-control-allow-origin"],
                true,
            )],
        );
        assert_eq!(messages.len(), 1);
        assert!(
            messages[0].contains("access-control-allow-origin"),
            "{messages:?}"
        );
    }

    #[test]
    fn a_header_written_by_both_a_builtin_and_a_plugin_is_reported() {
        let messages = conflicts(
            false,
            false,
            &["x-tenant".to_string()],
            &[plugin(
                "io.example.header-annotator",
                PluginKind::Annotate,
                &["x-tenant"],
                true,
            )],
        );
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("x-tenant"), "{messages:?}");
    }
}
