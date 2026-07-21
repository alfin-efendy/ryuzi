//! First-party Atlassian connector component.
//!
//! Exports `ryuzi:connector/connector@0.1.0` (the Task 9 adapter's contract:
//! `list-tools` + `invoke`) and speaks the Jira Cloud + Confluence Cloud REST
//! APIs behind the host-managed `ryuzi:oauth/oauth` capability — the host
//! injects the bearer, so this component never sees an access or refresh
//! token and never drives the 3LO authorize/token exchange (Cockpit + the
//! host own that; see `crates/core/src/plugins/capabilities/oauth.rs`).
//!
//! # One profile, two products
//! Atlassian Cloud's OAuth 2.0 (3LO) grant is per-app, not per-product: a
//! single `atlassian-cloud` authorization covers every Jira/Confluence site
//! the granted scopes allow. All ten tools — Jira and Confluence alike — call
//! `oauth.authorized-request("atlassian-cloud", ..)`. Every request is routed
//! through the API gateway `https://api.atlassian.com/ex/{jira,confluence}/{cloud_id}/...`
//! (never a tenant's own `*.atlassian.net` host directly), so a `cloud_id`
//! argument is required on every Jira/Confluence tool. `auth_status` (`GET
//! /oauth/token/accessible-resources`) is how a caller discovers which
//! `cloud_id`s the connection can reach.
//!
//! # Why `cloud_id` is a tool argument, not resolved-and-cached
//! This bundle's `lifecycle` is `per-call` (see `ryuzi-plugin.toml`): the
//! host spins up a fresh component instance for every `invoke`, so there is
//! nowhere in-process to cache a resolved `cloud_id` between calls even if
//! the component wanted to. Requiring it as an explicit argument is therefore
//! the only approach consistent with the lifecycle, not just the simplest
//! one — see the [`logic`] module doc for the full accounting.
//!
//! # Architecture: pure `logic` vs. wasm `guest`
//! Every decision that does not need a live host — which HTTP request each
//! tool builds, how each response is parsed, and the confirmation gate that
//! refuses an unconfirmed mutation *before any request is built* — lives in
//! the [`logic`] module as pure functions over plain Rust types, so it is
//! exercised entirely by native `cargo test` without a wasm host. The `guest`
//! module (compiled only for `wasm32`) is thin glue: it maps the WIT
//! `tool-call` into [`logic`]'s plain types, hands the planned request to
//! `oauth.authorized-request("atlassian-cloud", ..)`, and maps the response
//! back into the WIT `tool-result`.
//!
//! # Approval model
//! There is no approval field in the connector ABI (it stays `0.1.0`).
//! Instead, every mutating tool (`jira_issue_create`, `jira_issue_comment`,
//! `jira_issue_transition`, `confluence_page_create`, `confluence_page_update`)
//! takes a required boolean `confirm` argument; the pure logic returns
//! [`logic::ToolError::ConfirmationRequired`] (producing NO HTTP request)
//! whenever a mutating tool is called without `confirm=true`. This is a
//! component-internal guard that stacks on top of the host's own generic
//! per-tool approval prompt.

pub mod logic;

#[cfg(target_arch = "wasm32")]
mod guest;
