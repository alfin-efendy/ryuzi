//! First-party Qwen Code (OAuth) provider component.
//!
//! Exports `ryuzi:provider/provider@0.1.0` (`list-models` + `complete`) over
//! Qwen's OpenAI-compatible `/chat/completions` endpoint — the SAME wire format
//! as the OpenAI-chat API-key bundles, shared through the `ryuzi_openai_format`
//! crate. This bundle differs from those in exactly what makes it the Qwen
//! subscription provider rather than an API-key one:
//!
//! # A host-managed OAuth credential the component never sees
//! Qwen Code is an OAUTH provider: the credential is the user's Qwen
//! subscription obtained through an RFC 8628 device-authorization grant, not an
//! API key. This component therefore imports `ryuzi:oauth/oauth` INSTEAD of
//! `ryuzi:http/http` or `ryuzi:provider-auth/provider-auth`. Every request goes
//! through `oauth.authorized-request("qwen", ..)`, where the host resolves the
//! stored access token for the profile, injects it as `Authorization: Bearer …`
//! (stripping any the component set), enforces the manifest network allowlist
//! (including on every redirect hop), and returns only the upstream response.
//! The guest never reads, sets, or can forge the bearer — it has no plain HTTP
//! capability to try it with.
//!
//! # A seeded model list, not a fetch
//! The `qwen` descriptor sets `has_models_endpoint: false` (portal.qwen.ai's
//! `/models` route 404s), so `list-models` returns the descriptor's seeded model
//! list rather than fetching — see [`logic::SEEDED_MODELS`].
//!
//! # Accepted ABI limitation
//! `ryuzi:provider/provider` is flat text (a `prompt` string in, text chunks
//! out). This component therefore supports plain text completion only: no tool
//! calling, no structured multi-turn messages, no multimodal content, and no
//! true token streaming (the single upstream response is returned as one
//! terminal chunk). That is a deliberate, accepted tradeoff of the WASM provider
//! migration, not an oversight. See [`logic`] for the shard-`resource_url`
//! production gap this flat model cannot express.

pub mod logic;

#[cfg(target_arch = "wasm32")]
mod guest;
