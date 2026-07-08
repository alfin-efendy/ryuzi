# Changelog

## [0.3.0](https://github.com/alfin-efendy/ryuzi/compare/cockpit-v0.2.0...cockpit-v0.3.0) (2026-07-08)


### Features

* **cockpit:** chat enhancement batch — media in chat, turn summaries + edit cards, branch popover, model groups, open-in ([#75](https://github.com/alfin-efendy/ryuzi/issues/75)) ([3cce67c](https://github.com/alfin-efendy/ryuzi/commit/3cce67cf8cbf3596057423ba9e4bea434fb82c6c))
* **cockpit:** UI polish batch — solid overlays, Route groups, file viewer View/Code, Windows attachments, model Test All, instant sessions, git-URL/non-git projects ([#78](https://github.com/alfin-efendy/ryuzi/issues/78)) ([0cc9077](https://github.com/alfin-efendy/ryuzi/commit/0cc90770f35dcd6bf818d37f1003157ca3925d1a))
* **cockpit:** Windows bash fix, Ryuzi-only sessions, real branch controls, Combobox migration ([#72](https://github.com/alfin-efendy/ryuzi/issues/72)) ([9eb076d](https://github.com/alfin-efendy/ryuzi/commit/9eb076d315c1cdf237febd87a69d9b0a917ed0eb))
* **models:** provider category badges, free/free-tier providers, OpenAI-OAuth model fix (Phase A) ([#74](https://github.com/alfin-efendy/ryuzi/issues/74)) ([e6d269c](https://github.com/alfin-efendy/ryuzi/commit/e6d269cef013ba6d93ad965df3b5575bad2b01ec))
* **models:** Qwen Code + GitHub Copilot device-grant providers (Phase B) ([#76](https://github.com/alfin-efendy/ryuzi/issues/76)) ([8067a63](https://github.com/alfin-efendy/ryuzi/commit/8067a63f1fd2e79376ce47f0b5dbe406b384bc01))
* provider families + per-model router targets + HTTP endpoint failover ([#70](https://github.com/alfin-efendy/ryuzi/issues/70)) ([5e37347](https://github.com/alfin-efendy/ryuzi/commit/5e373477a74cf469f5bf33325b2873c50a859231))

## [0.2.0](https://github.com/alfin-efendy/ryuzi/compare/cockpit-v0.1.0...cockpit-v0.2.0) (2026-07-06)


### Features

* Kiro free tier — device flow + IDE import + AWS event-stream ([#55](https://github.com/alfin-efendy/ryuzi/issues/55)) ([f49a6e5](https://github.com/alfin-efendy/ryuzi/commit/f49a6e5e88a3c4177c514a619538af6e492cc54b))
* models & providers overhaul — provider detail views, capability-aware routing, account round-robin ([#56](https://github.com/alfin-efendy/ryuzi/issues/56)) ([f49a6e5](https://github.com/alfin-efendy/ryuzi/commit/f49a6e5e88a3c4177c514a619538af6e492cc54b))
* Models & Runtime F2b - OAuth (Claude sub + Codex) + OpenCode Free ([#51](https://github.com/alfin-efendy/ryuzi/issues/51)) ([4c47e9e](https://github.com/alfin-efendy/ryuzi/commit/4c47e9ebba4c6ef163a56e0f9d4f9352a135d160))
* native agent runtime — in-process harness, native tools, session export/import/share ([#54](https://github.com/alfin-efendy/ryuzi/issues/54)) ([f49a6e5](https://github.com/alfin-efendy/ryuzi/commit/f49a6e5e88a3c4177c514a619538af6e492cc54b))
* native runtime orchestration, parallel delegation & persistent memory ([#63](https://github.com/alfin-efendy/ryuzi/issues/63)) ([0cd9adc](https://github.com/alfin-efendy/ryuzi/commit/0cd9adc51d1f43cec2c625fd4222d01ae17d999c))
* OS-keychain credential encryption for router secrets ([#57](https://github.com/alfin-efendy/ryuzi/issues/57)) ([f49a6e5](https://github.com/alfin-efendy/ryuzi/commit/f49a6e5e88a3c4177c514a619538af6e492cc54b))
* plugin SDK — manifest contract v1, PluginHost, plugin-driven Cockpit menu, 24-integration catalog ([#58](https://github.com/alfin-efendy/ryuzi/issues/58)) ([f49a6e5](https://github.com/alfin-efendy/ryuzi/commit/f49a6e5e88a3c4177c514a619538af6e492cc54b))
* Rust architecture alignment — docs, design-system adoption, god-module splits, guardrails, tests ([#50](https://github.com/alfin-efendy/ryuzi/issues/50)) ([f49a6e5](https://github.com/alfin-efendy/ryuzi/commit/f49a6e5e88a3c4177c514a619538af6e492cc54b))


### Bug Fixes

* native runtime not working on chat cockpit ([#61](https://github.com/alfin-efendy/ryuzi/issues/61)) ([e02888f](https://github.com/alfin-efendy/ryuzi/commit/e02888f630212b42c6e36ab0ba3c2f8262da5b38))
* **release:** one combined GitHub release for CLI + Cockpit; unblock goreleaser, cockpit bundling, npm publish ([f49a6e5](https://github.com/alfin-efendy/ryuzi/commit/f49a6e5e88a3c4177c514a619538af6e492cc54b))

## 0.1.0 (2026-07-04)


### Features

* **cockpit:** readable chat transcript — markdown turns, thought & tool chips, live user bubble ([#30](https://github.com/alfin-efendy/ryuzi/issues/30)) ([a51e143](https://github.com/alfin-efendy/ryuzi/commit/a51e143e469957909c7875b4dfb851f4c688310e))
* **cockpit:** resizable persistent chat panels, multi-terminal drawer, honest settings ([#43](https://github.com/alfin-efendy/ryuzi/issues/43)) ([84c0359](https://github.com/alfin-efendy/ryuzi/commit/84c03592124a9147a76aa30bd204111b2ee94536))
* **cockpit:** Rust engine (R0) + Tauri v2 desktop app (R1) ([#17](https://github.com/alfin-efendy/ryuzi/issues/17)) ([e3c441b](https://github.com/alfin-efendy/ryuzi/commit/e3c441b87605788d825a9da295fe767a88855fab))
* **core, cockpit:** durable transcript history + extensibility architecture (Spec 1 & 2) ([#26](https://github.com/alfin-efendy/ryuzi/issues/26)) ([561ae52](https://github.com/alfin-efendy/ryuzi/commit/561ae52f9273a297aa63e95ae947aff03d5834d7))
* Models & Runtime — embedded local LLM router (Phase 1) ([#32](https://github.com/alfin-efendy/ryuzi/issues/32)) ([6a5ca2b](https://github.com/alfin-efendy/ryuzi/commit/6a5ca2ba09db0b577e88a9a1e7ec4414d321dc0c))
* Models & Runtime F2a - Responses API, usage tracking, mid-stream error fix ([#47](https://github.com/alfin-efendy/ryuzi/issues/47)) ([d60308a](https://github.com/alfin-efendy/ryuzi/commit/d60308a11e22fadd772d59cb09cc4f77197d2fce))
* rewrite CLI to Rust on the cockpit stack (Spec 4A–4D-b) ([#28](https://github.com/alfin-efendy/ryuzi/issues/28)) ([a9231bd](https://github.com/alfin-efendy/ryuzi/commit/a9231bd2a865f816431869d7da3f80dbf2b0e7ac))


### Miscellaneous Chores

* release 0.1.0 ([bec091d](https://github.com/alfin-efendy/ryuzi/commit/bec091d83bf544bed9e72db663e3664bf11e1e5b))
