//! Settings schema, provider catalog, and validated settings store facade.
//!
//! Transcribed verbatim from the retired TypeScript
//! `packages/core/src/config/{schema,required,store}.ts` and
//! `packages/core/src/providers/{types,catalog,gateways/discord,runtimes/claude-code}.ts`.

pub mod catalog;
pub mod fields;
pub mod store;

pub use catalog::{all_fields, find_field, is_secret, CATALOG};
pub use catalog::{GatewayDescriptor, ProviderCatalog, RuntimeDescriptor};
pub use fields::{ConfigField, FieldType, GLOBAL_FIELDS};
pub use store::{csv, validate_setting, SettingsStore};
