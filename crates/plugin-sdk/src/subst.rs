//! Placeholder substitution for plugin manifests: `${auth}`,
//! `${setting:KEY}`, and `${env:VAR}` are injected into MCP server
//! definitions (env values, headers, args, url) at session-attach time.
//! `$${` escapes to a literal `${` for manifests that need the raw syntax.

use thiserror::Error;

/// Supplies values for [`resolve`] to substitute into placeholders. Kept as
/// a trait object (`&dyn Resolver`) so callers can back it with settings /
/// secrets storage without this crate depending on that machinery.
pub trait Resolver {
    fn auth(&self) -> Option<String>;
    fn setting(&self, key: &str) -> Option<String>;
    fn env(&self, var: &str) -> Option<String>;
}

/// Errors from [`resolve`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SubstError {
    #[error("could not resolve placeholder {placeholder}")]
    Unresolved { placeholder: String },
}

/// Resolve every `${...}` placeholder in `input` via `r`. Supports
/// `${auth}`, `${setting:KEY}`, `${env:VAR}`; `$${` is an escape sequence
/// for a literal `${`. A single linear scan — no regex dependency.
pub fn resolve(input: &str, r: &dyn Resolver) -> Result<String, SubstError> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;

    while !rest.is_empty() {
        // `$${` escapes to a literal `${` and is not itself a placeholder.
        if let Some(after) = rest.strip_prefix("$${") {
            out.push_str("${");
            rest = after;
            continue;
        }

        if let Some(after_open) = rest.strip_prefix("${") {
            let Some(close_idx) = after_open.find('}') else {
                // No closing brace: the rest of the string is the
                // unresolved placeholder text.
                return Err(SubstError::Unresolved {
                    placeholder: rest.to_string(),
                });
            };
            let placeholder = &after_open[..close_idx];
            let raw = format!("${{{placeholder}}}");
            let value =
                resolve_one(placeholder, r).ok_or(SubstError::Unresolved { placeholder: raw })?;
            out.push_str(&value);
            rest = &after_open[close_idx + 1..];
            continue;
        }

        // Copy one char (by UTF-8 boundary) and keep scanning.
        let ch_len = rest.chars().next().map(char::len_utf8).unwrap_or(1);
        out.push_str(&rest[..ch_len]);
        rest = &rest[ch_len..];
    }

    Ok(out)
}

/// Resolve a single placeholder body (the text between `${` and `}`, e.g.
/// `auth`, `setting:plugin.x.y`, `env:FOO`) against `r`.
fn resolve_one(placeholder: &str, r: &dyn Resolver) -> Option<String> {
    if placeholder == "auth" {
        return r.auth();
    }
    if let Some(key) = placeholder.strip_prefix("setting:") {
        return r.setting(key);
    }
    if let Some(var) = placeholder.strip_prefix("env:") {
        return r.env(var);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct TestResolver {
        auth: Option<String>,
        settings: HashMap<String, String>,
        env: HashMap<String, String>,
    }

    impl Resolver for TestResolver {
        fn auth(&self) -> Option<String> {
            self.auth.clone()
        }

        fn setting(&self, key: &str) -> Option<String> {
            self.settings.get(key).cloned()
        }

        fn env(&self, var: &str) -> Option<String> {
            self.env.get(var).cloned()
        }
    }

    fn resolver() -> TestResolver {
        let mut settings = HashMap::new();
        settings.insert("plugin.x.y".to_string(), "setting-value".to_string());
        let mut env = HashMap::new();
        env.insert("FOO".to_string(), "env-value".to_string());
        TestResolver {
            auth: Some("auth-value".to_string()),
            settings,
            env,
        }
    }

    #[test]
    fn resolves_auth_placeholder() {
        assert_eq!(resolve("${auth}", &resolver()).unwrap(), "auth-value");
    }

    #[test]
    fn resolves_setting_placeholder() {
        assert_eq!(
            resolve("${setting:plugin.x.y}", &resolver()).unwrap(),
            "setting-value"
        );
    }

    #[test]
    fn resolves_env_placeholder() {
        assert_eq!(resolve("${env:FOO}", &resolver()).unwrap(), "env-value");
    }

    #[test]
    fn leaves_literal_text_untouched() {
        assert_eq!(
            resolve("just plain text, no placeholders here", &resolver()).unwrap(),
            "just plain text, no placeholders here"
        );
    }

    #[test]
    fn errors_on_unresolved_placeholder() {
        let err = resolve("${env:MISSING}", &resolver()).unwrap_err();
        assert_eq!(
            err,
            SubstError::Unresolved {
                placeholder: "${env:MISSING}".to_string()
            }
        );
    }

    #[test]
    fn errors_on_unknown_placeholder_kind() {
        let err = resolve("${nonsense}", &resolver()).unwrap_err();
        assert_eq!(
            err,
            SubstError::Unresolved {
                placeholder: "${nonsense}".to_string()
            }
        );
    }

    #[test]
    fn resolves_multiple_placeholders_in_one_string() {
        let input = "Bearer ${auth} for ${env:FOO} and ${setting:plugin.x.y}";
        assert_eq!(
            resolve(input, &resolver()).unwrap(),
            "Bearer auth-value for env-value and setting-value"
        );
    }

    #[test]
    fn escapes_dollar_dollar_brace_to_literal() {
        assert_eq!(resolve("$${auth}", &resolver()).unwrap(), "${auth}");
    }

    #[test]
    fn escape_mixed_with_real_placeholder() {
        assert_eq!(
            resolve("literal $${auth} then real ${auth}", &resolver()).unwrap(),
            "literal ${auth} then real auth-value"
        );
    }
}
