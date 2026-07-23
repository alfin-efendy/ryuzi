//! First-party Google (Gemini) provider component.
//!
//! Exports `ryuzi:provider/provider@0.1.0` (`list-models` + `complete`) over
//! Gemini's OpenAI-COMPATIBILITY endpoint
//! (`https://generativelanguage.googleapis.com/v1beta/openai`), whose `/models`
//! and `/chat/completions` routes speak the same wire format every other
//! component in this family does. The native Gemini `generateContent` API is a
//! different protocol and is deliberately not what this component talks to —
//! the `google` descriptor names the compatibility base URL, and this component
//! follows it.
//!
//! # The component never sees the user's API key
//! Google is an API-KEY provider: the credential belongs to the user, not the
//! bundle. This component therefore imports `ryuzi:provider-auth/provider-auth`
//! INSTEAD of `ryuzi:http/http`. The host resolves the stored key, injects it
//! per the `google` descriptor's `AuthScheme::Bearer`, enforces the manifest
//! network allowlist (including on every redirect hop), and returns only the
//! upstream response. The guest never reads, sets, or can forge an
//! `Authorization` header — it has no plain HTTP capability to try it with.
//!
//! # Architecture: shared wire logic vs. per-provider config
//! All wire behaviour lives in the shared `ryuzi_openai_format` crate and is
//! tested there natively. This crate is two thin pieces on top:
//! [`logic::CONFIG`], the `google` descriptor transcribed into that crate's
//! `OpenAiFormat`, and a `guest` module (compiled only for `wasm32`) that is a
//! single `provider_component!` invocation.
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
