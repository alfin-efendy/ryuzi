//! First-party OpenAI provider component.
//!
//! Exports `ryuzi:provider/provider@0.1.0` (the Task 10 adapter's contract:
//! `list-models` + `complete`) over OpenAI's `/v1/models` and
//! `/v1/chat/completions` endpoints.
//!
//! # The component never sees the user's API key
//! Unlike the free-tier `mimo`/`opencode` components, OpenAI is an
//! API-KEY provider: the credential belongs to the user, not the bundle. This
//! component therefore imports `ryuzi:provider-auth/provider-auth` INSTEAD of
//! `ryuzi:http/http`. The host resolves the stored key, injects it per the
//! `openai` descriptor's `AuthScheme::Bearer`, enforces the manifest network
//! allowlist (including on every redirect hop), and returns only the upstream
//! response. The guest never reads, sets, or can forge an `Authorization`
//! header — it has no plain HTTP capability to try it with.
//!
//! # Architecture: shared wire logic vs. per-provider config
//! Every piece of behaviour that does not need a live host — base-URL
//! resolution, request-body shaping, response/model parsing, upstream-status
//! classification — lives in the shared `ryuzi_openai_format` crate as pure
//! functions over plain Rust types, so the whole wire mapping is exercised by
//! that crate's native `cargo test` without a wasm host. This crate is two thin
//! pieces on top: [`logic::CONFIG`], the `openai` descriptor transcribed into
//! that crate's `OpenAiFormat`, and a `guest` module (compiled only for
//! `wasm32`) that is a single `provider_component!` invocation.
//!
//! # Base-URL override
//! The upstream base defaults to the descriptor's `base_url`
//! (`https://api.openai.com/v1`) but is overridden by a non-empty value at the
//! `ryuzi_openai_format::BASE_URL_STORAGE_KEY` key of this component's own
//! (host-scoped) `ryuzi:storage` slice. That serves two real purposes: pointing
//! the component at an OpenAI-compatible proxy or gateway, and letting the
//! provider conformance harness aim it at a loopback mock upstream. The
//! manifest network allowlist still applies to whatever the override resolves
//! to, so an override can never widen where the user's key may travel.
//!
//! # Accepted ABI limitation
//! `ryuzi:provider/provider` is flat text (a `prompt` string in, text chunks
//! out). This component therefore supports plain text completion only: no tool
//! calling, no structured multi-turn messages, no multimodal content, and no
//! true token streaming (the single upstream response is returned as one
//! terminal chunk). That is a deliberate, accepted tradeoff of the WASM
//! provider migration, not an oversight.

pub mod logic;

#[cfg(target_arch = "wasm32")]
mod guest;
