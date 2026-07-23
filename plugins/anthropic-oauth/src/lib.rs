//! First-party Anthropic (Claude subscription) provider component.
//!
//! Exports `ryuzi:provider/provider@0.1.0` (`list-models` + `complete`) over
//! Anthropic's `/models` and `/messages` endpoints — the SAME wire format as the
//! `anthropic` (x-api-key) bundle, shared through the `ryuzi_anthropic_format`
//! crate. This bundle differs in exactly two ways, both of which are what make
//! it the Claude-subscription provider rather than the API-key one:
//!
//! # A host-managed OAuth credential the component never sees
//! Claude Pro/Max is an OAUTH provider: the credential is the user's Claude
//! subscription, not an API key. This component therefore imports
//! `ryuzi:oauth/oauth` INSTEAD of `ryuzi:http/http` or
//! `ryuzi:provider-auth/provider-auth`. Every request goes through
//! `oauth.authorized-request("anthropic-oauth", ..)`, where the host resolves
//! the stored access token for the profile, injects it as `Authorization:
//! Bearer …` (stripping any the component set), enforces the manifest network
//! allowlist (including on every redirect hop), and returns only the upstream
//! response. The guest never reads, sets, or can forge the bearer — it has no
//! plain HTTP capability to try it with.
//!
//! # The Claude-subscription auth markers
//! Anthropic's OAuth endpoint accepts a subscription bearer only when the
//! request also carries the Claude-Code auth markers the official client sends:
//! the `anthropic-beta` OAuth flag, the `?beta=true` query, and a leading
//! Claude-Code system prompt. Those are ported verbatim from the native router
//! path (`llm_router::models` / `llm_router::client::oauth_upstream_request`);
//! see [`logic`] for exactly which markers, and for the anti-abuse spoofing the
//! flat-text ABI deliberately omits.
//!
//! # Accepted ABI limitation
//! `ryuzi:provider/provider` is flat text (a `prompt` string in, text chunks
//! out). This component therefore supports plain text completion only: no tool
//! calling, no structured multi-turn messages, no multimodal content, and no
//! true token streaming (the single upstream response is returned as one
//! terminal chunk). That is a deliberate, accepted tradeoff of the WASM provider
//! migration, not an oversight.

pub mod logic;

#[cfg(target_arch = "wasm32")]
mod guest;
