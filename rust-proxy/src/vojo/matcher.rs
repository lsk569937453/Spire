use http::{HeaderMap, Method};
use once_cell::sync::OnceCell;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;

/// Lazily compiled regex that can be shared between clones of a matcher rule.
///
/// Compiling a regex costs microseconds, so it must never happen on the request
/// path more than once per pattern. `MatcherRule` is cloned (config snapshots,
/// logging), therefore the compiled regex lives behind an `Arc<OnceCell<..>>`:
/// the first match compiles it, all later matches and all clones reuse it.
///
/// `OnceCell::get_or_init` only needs `&self`, which is what allows routing to
/// run while holding a *shared* (read) lock on the configuration.
#[derive(Debug, Clone, Default)]
pub struct RegexCell(Arc<OnceCell<Option<Regex>>>);

impl RegexCell {
    #[allow(dead_code)] // used by tests and by future config normalisation
    pub fn new() -> Self {
        Self::default()
    }

    /// Compiles `pattern` on first use and caches the result (including the
    /// failure case, so an invalid pattern is not re-compiled on every request).
    pub fn get_or_compile(&self, pattern: &str) -> Option<&Regex> {
        self.0
            .get_or_init(|| match Regex::new(pattern) {
                Ok(re) => Some(re),
                Err(e) => {
                    error!("Invalid regex pattern '{pattern}': {e}");
                    None
                }
            })
            .as_ref()
    }

    #[allow(dead_code)] // used by tests
    pub fn compiled(&self) -> Option<&Regex> {
        self.0.get().and_then(|item| item.as_ref())
    }
}

impl PartialEq for RegexCell {
    /// The compiled regex is derived state; two rules with the same pattern are
    /// equal even if only one of them has compiled it yet.
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

/// Host and header matchers are anchored before being compiled.
///
/// Without anchoring, `value: example.com` (or any unanchored pattern) matches
/// `example.com.attacker.tld`, which is a host-confusion / routing-bypass bug
/// for virtual-host based routing. `^(?:..)$` keeps existing anchored patterns
/// working unchanged.
fn anchored(pattern: &str) -> String {
    format!("^(?:{pattern})$")
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub enum PathMatchType {
    #[serde(rename = "prefix")]
    #[default]
    Prefix,
    #[serde(rename = "exact")]
    Exact,
    #[serde(rename = "regex")]
    Regex,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "PascalCase")]
pub enum MatcherRule {
    #[serde(rename = "path")]
    Path {
        value: String,
        #[serde(default)]
        match_type: PathMatchType,
        #[serde(skip)]
        regex: RegexCell,
    },
    #[serde(rename = "host")]
    Host {
        value: String,
        #[serde(skip)]
        regex: RegexCell,
    },
    #[serde(rename = "header")]
    Header {
        name: String,
        value: String,
        #[serde(skip)]
        regex: RegexCell,
    },
    #[serde(rename = "method")]
    Method { values: HashSet<String> },
}

impl PartialEq for MatcherRule {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Path {
                    value: l_val,
                    match_type: l_mt,
                    ..
                },
                Self::Path {
                    value: r_val,
                    match_type: r_mt,
                    ..
                },
            ) => l_val == r_val && l_mt == r_mt,

            (Self::Host { value: l_val, .. }, Self::Host { value: r_val, .. }) => l_val == r_val,

            (
                Self::Header {
                    name: l_name,
                    value: l_val,
                    ..
                },
                Self::Header {
                    name: r_name,
                    value: r_val,
                    ..
                },
            ) => l_name == r_name && l_val == r_val,
            (Self::Method { values: l_vals }, Self::Method { values: r_vals }) => l_vals == r_vals,
            _ => false,
        }
    }
}

impl MatcherRule {
    /// Returns the compiled regex of a `path` matcher, compiling it on first
    /// use. `path_rewrite` reuses this instead of compiling the same pattern a
    /// second time for every request.
    pub fn path_regex(&self) -> Option<&Regex> {
        match self {
            MatcherRule::Path { value, regex, .. } => regex.get_or_compile(value),
            _ => None,
        }
    }

    pub fn matches(&self, method: &Method, path: &str, headers: &HeaderMap) -> bool {
        match self {
            MatcherRule::Path {
                value,
                match_type,
                regex,
            } => match match_type {
                PathMatchType::Prefix => {
                    if path.starts_with(value.as_str()) {
                        true
                    } else {
                        debug!(
                            "Path matching failed: path '{path}' does not start with prefix '{value}'"
                        );
                        false
                    }
                }
                PathMatchType::Exact => {
                    if path == value.as_str() {
                        true
                    } else {
                        debug!(
                            "Path matching failed: path '{path}' does not exactly match '{value}'"
                        );
                        false
                    }
                }
                PathMatchType::Regex => match regex.get_or_compile(value) {
                    Some(re) => {
                        if re.is_match(path) {
                            true
                        } else {
                            debug!(
                                "Path matching failed: path '{path}' does not match regex '{value}'"
                            );
                            false
                        }
                    }
                    None => false,
                },
            },
            MatcherRule::Method { values } => {
                if values.contains(method.as_str()) {
                    true
                } else {
                    debug!(
                        "Method matching failed: method '{method}' is not in the allowed list '{values:?}'"
                    );
                    false
                }
            }
            MatcherRule::Host { value, regex } => {
                let Some(host_header) = headers.get(http::header::HOST) else {
                    debug!("Host matching failed: 'Host' header not found");
                    return false;
                };
                let Ok(host) = host_header.to_str() else {
                    debug!(
                        "Host matching failed: 'Host' header contains non-visible ASCII characters"
                    );
                    return false;
                };
                match regex.get_or_compile(&anchored(value)) {
                    Some(re) => {
                        if re.is_match(host) {
                            true
                        } else {
                            debug!(
                                "Host matching failed: host '{host}' does not match pattern '{value}'"
                            );
                            false
                        }
                    }
                    None => false,
                }
            }
            MatcherRule::Header { name, value, regex } => {
                let Some(header_value) = headers.get(name.as_str()) else {
                    debug!("Header matching failed: header '{name}' not found");
                    return false;
                };
                let Ok(header_value) = header_value.to_str() else {
                    debug!(
                        "Header matching failed: header '{name}' contains non-visible ASCII characters"
                    );
                    return false;
                };
                match regex.get_or_compile(&anchored(value)) {
                    Some(re) => {
                        if re.is_match(header_value) {
                            true
                        } else {
                            debug!(
                                "Header matching failed: header '{name}' with value '{header_value}' does not match pattern '{value}'"
                            );
                            false
                        }
                    }
                    None => false,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::{HeaderValue, Method, header};
    use std::collections::HashSet;

    fn path_rule(value: &str, match_type: PathMatchType) -> MatcherRule {
        MatcherRule::Path {
            value: value.to_string(),
            match_type,
            regex: RegexCell::new(),
        }
    }

    #[test]
    fn test_path_prefix_match_success() {
        let rule = path_rule("/api/v1", PathMatchType::Prefix);
        let headers = HeaderMap::new();
        assert!(rule.matches(&Method::GET, "/api/v1/users", &headers));
    }

    #[test]
    fn test_path_prefix_match_failure() {
        let rule = path_rule("/api/v1", PathMatchType::Prefix);
        let headers = HeaderMap::new();
        assert!(!rule.matches(&Method::GET, "/app/v1/users", &headers));
    }

    #[test]
    fn test_path_exact_match_success() {
        let rule = path_rule("/api/v1/users", PathMatchType::Exact);
        let headers = HeaderMap::new();
        assert!(rule.matches(&Method::GET, "/api/v1/users", &headers));
    }

    #[test]
    fn test_path_exact_match_failure() {
        let rule = path_rule("/api/v1/users", PathMatchType::Exact);
        let headers = HeaderMap::new();
        assert!(!rule.matches(&Method::GET, "/api/v1/users/1", &headers));
    }

    #[test]
    fn test_path_regex_match_success() {
        let rule = path_rule("^/users/\\d+$", PathMatchType::Regex);
        let headers = HeaderMap::new();
        assert!(rule.matches(&Method::GET, "/users/123", &headers));
    }

    #[test]
    fn test_path_regex_match_failure() {
        let rule = path_rule("^/users/\\d+$", PathMatchType::Regex);
        let headers = HeaderMap::new();
        assert!(!rule.matches(&Method::GET, "/users/abc", &headers));
    }

    #[test]
    fn test_path_regex_invalid_pattern() {
        let rule = path_rule("[", PathMatchType::Regex);
        let headers = HeaderMap::new();
        assert!(!rule.matches(&Method::GET, "/users/123", &headers));
        // The failure is cached, matching again must not re-compile or panic.
        assert!(!rule.matches(&Method::GET, "/users/456", &headers));
        assert!(matches!(&rule, MatcherRule::Path { regex, .. } if regex.compiled().is_none()));
    }

    #[test]
    fn test_method_match_success() {
        let rule = MatcherRule::Method {
            values: HashSet::from(["GET".to_string(), "POST".to_string()]),
        };
        let headers = HeaderMap::new();
        assert!(rule.matches(&Method::GET, "/any/path", &headers));
    }

    #[test]
    fn test_method_match_failure() {
        let rule = MatcherRule::Method {
            values: HashSet::from(["GET".to_string(), "POST".to_string()]),
        };
        let headers = HeaderMap::new();
        assert!(!rule.matches(&Method::PUT, "/any/path", &headers));
    }

    #[test]
    fn test_host_match_success() {
        let rule = MatcherRule::Host {
            value: r"^(api|www)\.example\.com$".to_string(),
            regex: RegexCell::new(),
        };
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("api.example.com"));
        assert!(rule.matches(&Method::GET, "/", &headers));
    }

    #[test]
    fn test_host_match_failure() {
        let rule = MatcherRule::Host {
            value: r"^(api|www)\.example\.com$".to_string(),
            regex: RegexCell::new(),
        };
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("other.example.com"));
        assert!(!rule.matches(&Method::GET, "/", &headers));
    }

    #[test]
    fn test_host_match_is_anchored() {
        // Regression test: an unanchored pattern used to match any host that
        // merely *contains* it, which allowed host-based routing bypasses.
        let rule = MatcherRule::Host {
            value: "example.com".to_string(),
            regex: RegexCell::new(),
        };

        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("example.com"));
        assert!(rule.matches(&Method::GET, "/", &headers));

        headers.insert(
            header::HOST,
            HeaderValue::from_static("example.com.attacker.tld"),
        );
        assert!(!rule.matches(&Method::GET, "/", &headers));
    }

    #[test]
    fn test_host_match_header_missing() {
        let rule = MatcherRule::Host {
            value: r"^(api|www)\.example\.com$".to_string(),
            regex: RegexCell::new(),
        };
        let headers = HeaderMap::new();
        assert!(!rule.matches(&Method::GET, "/", &headers));
    }

    #[test]
    fn test_host_match_invalid_regex() {
        let rule = MatcherRule::Host {
            value: "[invalid".to_string(),
            regex: RegexCell::new(),
        };
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("api.example.com"));
        assert!(!rule.matches(&Method::GET, "/", &headers));
    }

    #[test]
    fn test_header_match_success() {
        let rule = MatcherRule::Header {
            name: "x-request-id".to_string(),
            value: r"^token-\d+$".to_string(),
            regex: RegexCell::new(),
        };
        let mut headers = HeaderMap::new();
        headers.insert("x-request-id", HeaderValue::from_static("token-12345"));
        assert!(rule.matches(&Method::GET, "/", &headers));
    }

    #[test]
    fn test_header_match_failure_value() {
        let rule = MatcherRule::Header {
            name: "x-request-id".to_string(),
            value: r"^token-\d+$".to_string(),
            regex: RegexCell::new(),
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-request-id",
            HeaderValue::from_static("invalid-token-format"),
        );
        assert!(!rule.matches(&Method::GET, "/", &headers));
    }

    #[test]
    fn test_header_match_failure_missing() {
        let rule = MatcherRule::Header {
            name: "x-request-id".to_string(),
            value: r"^token-\d+$".to_string(),
            regex: RegexCell::new(),
        };
        let headers = HeaderMap::new();
        assert!(!rule.matches(&Method::GET, "/", &headers));
    }

    #[test]
    fn test_header_match_invalid_regex() {
        let rule = MatcherRule::Header {
            name: "x-request-id".to_string(),
            value: "[invalid".to_string(),
            regex: RegexCell::new(),
        };
        let mut headers = HeaderMap::new();
        headers.insert("x-request-id", HeaderValue::from_static("token-12345"));
        assert!(!rule.matches(&Method::GET, "/", &headers));
    }

    #[test]
    fn test_partialeq_path_equal() {
        let r1 = path_rule("/path", PathMatchType::Prefix);
        let r2 = path_rule("/path", PathMatchType::Prefix);
        assert_eq!(r1, r2);
    }

    #[test]
    fn test_partialeq_path_not_equal() {
        let r1 = path_rule("/path", PathMatchType::Prefix);
        let r2 = path_rule("/path", PathMatchType::Exact);
        assert_ne!(r1, r2);
    }

    #[test]
    fn test_partialeq_different_variants() {
        let r1 = path_rule("/path", PathMatchType::Prefix);
        let r2 = MatcherRule::Host {
            value: "example.com".to_string(),
            regex: RegexCell::new(),
        };
        assert_ne!(r1, r2);
    }

    #[test]
    fn test_partialeq_ignores_cached_regex() {
        let r1 = path_rule("^/users/\\d+$", PathMatchType::Regex);
        let r2 = path_rule("^/users/\\d+$", PathMatchType::Regex);

        assert_eq!(r1, r2);

        let headers = HeaderMap::new();
        assert!(r1.matches(&Method::GET, "/users/123", &headers));

        assert!(matches!(&r1, MatcherRule::Path { regex, .. } if regex.compiled().is_some()));
        assert!(matches!(&r2, MatcherRule::Path { regex, .. } if regex.compiled().is_none()));

        assert_eq!(r1, r2);
    }

    #[test]
    fn test_path_regex_is_only_compiled_once() {
        let rule = path_rule("^/users/\\d+$", PathMatchType::Regex);
        let headers = HeaderMap::new();

        // Two concurrent style accesses: the cell must be initialised after the
        // first call and stay initialised.
        assert!(rule.matches(&Method::GET, "/users/1", &headers));
        assert!(matches!(&rule, MatcherRule::Path { regex, .. } if regex.compiled().is_some()));
        assert!(rule.matches(&Method::GET, "/users/2", &headers));
    }
}
