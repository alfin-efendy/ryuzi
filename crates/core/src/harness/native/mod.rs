//! Native agent runtime.
//!
//! Unlike the ACP harness ([`super::acp`]), which delegates all reasoning and
//! tool execution to an external Claude Code adapter process, the native
//! runtime runs the agentic loop in-process: it calls LLMs through
//! [`crate::llm_router::client`], executes its own built-in tools
//! ([`tools`]), enforces permissions ([`permission`]), and persists a
//! provider-turn ledger ([`ledger`]) — registered under the harness id
//! `"native"` beside `"claude-code"`.
//!
//! See `docs/design/2026-07-05-native-agent-runtime-design.md`.

pub mod permission;
pub mod tools;
// context, ledger, llm, runner, and the NativeHarness itself are added in
// later slices of the Phase 1 plan.
