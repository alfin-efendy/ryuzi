//! First-party Bitbucket connector component.
//!
//! Exports `ryuzi:connector/connector@0.1.0` (the Task 9 adapter's contract:
//! `list-tools` + `invoke`) and speaks the Bitbucket Cloud REST API 2.0
//! behind the host-managed `ryuzi:oauth/oauth` capability — the host injects
//! the bearer, so this component never sees an access or refresh token and
//! never drives the OAuth authorize/token exchange (Cockpit + the host own
//! that; see `crates/core/src/plugins/capabilities/oauth.rs`).
//!
//! # A DISTINCT profile from Atlassian
//! This is the whole point of splitting `bitbucket` out as its own bundle
//! (see Task 15/15b/15c): Bitbucket Cloud's OAuth consumer is registered and
//! granted independently of Jira/Confluence's `atlassian-cloud` 3LO app —
//! the two products live under the same Atlassian umbrella but do NOT share
//! an OAuth grant. Every tool here calls
//! `oauth.authorized-request("bitbucket-cloud", ..)`; it is a bug for this
//! component to ever reference `"atlassian-cloud"`, and Task 15c's isolation
//! tests assert exactly that (every Bitbucket request fails without its own
//! `bitbucket-cloud` connection, even if Atlassian is already connected).
//!
//! # Why `workspace`/`repo_slug` are tool arguments, not resolved-and-cached
//! This bundle's `lifecycle` is `per-call` (see `ryuzi-plugin.toml`): the
//! host spins up a fresh component instance for every `invoke`, so there is
//! nowhere in-process to cache a resolved workspace/repo between calls even
//! if the component wanted to. Requiring them as explicit arguments on every
//! repo/PR/issue tool is therefore the only approach consistent with the
//! lifecycle, mirroring how the Atlassian connector requires `cloud_id`.
//!
//! # Architecture: pure `logic` vs. wasm `guest`
//! Every decision that does not need a live host — which HTTP request each
//! tool builds, how each response is parsed, and the confirmation gate that
//! refuses an unconfirmed mutation *before any request is built* — lives in
//! the [`logic`] module as pure functions over plain Rust types, so it is
//! exercised entirely by native `cargo test` without a wasm host. The `guest`
//! module (compiled only for `wasm32`) is thin glue: it maps the WIT
//! `tool-call` into [`logic`]'s plain types, hands the planned request to
//! `oauth.authorized-request("bitbucket-cloud", ..)`, and maps the response
//! back into the WIT `tool-result`.
//!
//! # Approval model
//! There is no approval field in the connector ABI (it stays `0.1.0`).
//! Instead, every mutating tool (`pr_create`, `pr_merge`, `pr_comment`,
//! `issue_create`) takes a required boolean `confirm` argument; the pure
//! logic returns [`logic::ToolError::ConfirmationRequired`] (producing NO
//! HTTP request) whenever a mutating tool is called without `confirm=true`.
//! This is a component-internal guard that stacks on top of the host's own
//! generic per-tool approval prompt.

pub mod logic;

#[cfg(target_arch = "wasm32")]
mod guest;
