//! Plugin binding layer.
//!
//! A [`ryuzi_plugin_sdk::PluginManifest`] is purely declarative — identity,
//! metadata, settings schema, and so on. This module binds a manifest to the
//! behavioral capabilities it actually provides at runtime
//! ([`host::CorePlugin`]), tracks every installed plugin
//! ([`host::PluginHost`]), and is the new home of [`host::Registries`] (moved
//! here from the deleted `integration` module, which this layer replaces
//! entirely — see `host`'s module doc for how `Registries::add_plugin`
//! supersedes the old `Integration` trait).
//!
//! [`builtin`] holds first-party plugins that don't have a more natural home
//! beside their own implementation module — `native`/`claude-code` live
//! beside their harness code in `harness::native`/`harness::acp`; `discord`
//! lives here since `gateway::discord` is data/protocol-only.

pub mod builtin;
pub mod host;

pub use host::{CorePlugin, PluginHost, PluginSource, Registries};
