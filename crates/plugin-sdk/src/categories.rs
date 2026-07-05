//! The standard category (label) vocabulary plugins describe themselves
//! with. The list is deliberately extensible: `PluginManifest::warnings`
//! flags categories outside `KNOWN` rather than rejecting them, so an
//! unrecognized label warns instead of breaking the loader.

/// Standard plugin category labels. The Cockpit catalog filters plugins by
/// these; unknown categories are not a parse error, only a warning surfaced
/// via `crate::manifest::PluginManifest::warnings()`.
pub const KNOWN: &[&str] = &[
    "model-provider",
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
