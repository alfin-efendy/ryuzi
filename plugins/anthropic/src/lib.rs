//! First-party Anthropic provider component.
//!
//! Exports `ryuzi:provider/provider@0.1.0` (`list-models` + `complete`) over
//! Anthropic's `/models` and `/messages` endpoints.
//!
//! # A different wire format
//! Every other ported provider component speaks OpenAI-chat and is built on the
//! shared `ryuzi-openai-format` crate. The `anthropic` descriptor declares
//! `ApiFormat::Anthropic`, which is a genuinely different protocol: generation
//! POSTs to `/messages` with a REQUIRED `max_tokens`, the response is a
//! `content[]` array of typed blocks rather than `choices[]`, usage is
//! `input_tokens`/`output_tokens`, errors carry `error.type`, and every request
//! must name an `anthropic-version`. That logic lives in [`logic`] — see its
//! module doc for why it is not a flag on the OpenAI-format crate.
//!
//! # The component never sees the user's API key
//! Anthropic is an API-KEY provider: the credential belongs to the user, not
//! the bundle. This component therefore imports
//! `ryuzi:provider-auth/provider-auth` INSTEAD of `ryuzi:http/http`. The host
//! resolves the stored key, injects it per the `anthropic` descriptor's
//! `AuthScheme::XApiKey` (i.e. as `x-api-key`), enforces the manifest network
//! allowlist (including on every redirect hop), and returns only the upstream
//! response. The guest never reads, sets, or can forge a credential header — it
//! has no plain HTTP capability to try it with, and the host discards any
//! credential-shaped header a component supplies before injecting its own.
//!
//! The `anthropic-version` header the guest DOES set is a protocol version, not
//! a credential; see [`logic::ANTHROPIC_VERSION`].
//!
//! # Accepted ABI limitation
//! `ryuzi:provider/provider` is flat text (a `prompt` string in, text chunks
//! out). This component therefore supports plain text completion only: no tool
//! calling, no system prompt, no structured multi-turn messages, no multimodal
//! content, and no true token streaming (the single upstream response is
//! returned as one terminal chunk). That is a deliberate, accepted tradeoff of
//! the WASM provider migration, not an oversight.

pub mod logic;

#[cfg(target_arch = "wasm32")]
mod guest;
