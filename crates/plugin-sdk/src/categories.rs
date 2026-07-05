//! The standard category (label) vocabulary plugins describe themselves
//! with. The list is deliberately extensible: `PluginManifest::warnings`
//! flags categories outside `KNOWN` rather than rejecting them, so an
//! unrecognized label warns instead of breaking the loader.

/// Standard plugin category labels. The Cockpit catalog filters plugins by
/// these; unknown categories are not a parse error, only a warning surfaced
/// via `crate::manifest::PluginManifest::warnings()`.
pub const KNOWN: &[&str] = &[
    "model-provider",
    "api-key",
    "oauth",
    "free",
    "runtime",
    "cli-agent",
    "chat-gateway",
    "vcs",
    "issues",
    "docs",
    "wiki",
    "productivity",
    "memory",
    "knowledge-graph",
    "search",
    "design",
    "observability",
    "sandbox",
    "tunnel",
    "deploy",
    "communication",
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn known_has_exactly_twenty_one_entries() {
        assert_eq!(KNOWN.len(), 21);
    }

    #[test]
    fn known_has_no_duplicates() {
        let unique: HashSet<&&str> = KNOWN.iter().collect();
        assert_eq!(unique.len(), KNOWN.len());
    }

    #[test]
    fn known_entries_are_kebab_case() {
        for category in KNOWN {
            assert!(
                category.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
                "category {category:?} is not kebab-case"
            );
        }
    }
}
