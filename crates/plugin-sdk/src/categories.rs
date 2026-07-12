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
    "skills",
];

/// Slot names with first-class arbitration support in `ryuzi-core`'s plugin
/// slot registry (`PluginHost::slot_owner`, Track C Feature C2) — a slot is
/// a named capability exactly one installed plugin may claim (e.g. the
/// roadmap's `plugins.slots.memory`, feeding Hermes memory backends).
/// Deliberately a narrow, mirrored subset of [`KNOWN`]: only capabilities
/// meaningful as an exclusive singleton belong here, not every cosmetic
/// category. Like `KNOWN`, this list is not exhaustive validation — an
/// unknown slot name is a warning, not a manifest error (see
/// `crate::manifest::PluginManifest::warnings`).
pub const KNOWN_SLOTS: &[&str] = &["memory", "knowledge-graph", "search"];

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

    #[test]
    fn known_slots_are_a_subset_of_known_categories() {
        for slot in KNOWN_SLOTS {
            assert!(
                KNOWN.contains(slot),
                "slot {slot:?} should also be a known category"
            );
        }
    }

    #[test]
    fn known_slots_has_no_duplicates() {
        let unique: HashSet<&&str> = KNOWN_SLOTS.iter().collect();
        assert_eq!(unique.len(), KNOWN_SLOTS.len());
    }
}
