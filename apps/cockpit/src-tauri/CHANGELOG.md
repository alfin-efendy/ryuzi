# Changelog

## [0.5.0](https://github.com/alfin-efendy/ryuzi/compare/cockpit-v0.4.0...cockpit-v0.5.0) (2026-07-24)


### Features

* Phase 6 — GitHub connector, WASM WebSocket capability + Discord gateway migration, Atlassian/Bitbucket connectors ([#165](https://github.com/alfin-efendy/ryuzi/issues/165)) ([e9caca2](https://github.com/alfin-efendy/ryuzi/commit/e9caca2dcc9b8cd8f0a91ded6d8b42e9595a1944))
* **plugins:** component catalog migration + end-to-end OAuth connect ([#169](https://github.com/alfin-efendy/ryuzi/issues/169)) ([ba6612a](https://github.com/alfin-efendy/ryuzi/commit/ba6612a93f356e6b8be5cbf2221415954603e55c))
* WASM Component Plugin Platform — signed bundles, generic adapters, bootstrap providers & Cockpit UX (Phases 1–5) ([#163](https://github.com/alfin-efendy/ryuzi/issues/163)) ([4993ae1](https://github.com/alfin-efendy/ryuzi/commit/4993ae1b81a488490669abf2c72d2b2625ce3313))

## [0.4.0](https://github.com/alfin-efendy/ryuzi/compare/cockpit-v0.3.0...cockpit-v0.4.0) (2026-07-18)


### ⚠ BREAKING CHANGES

* **cockpit:** remove legacy agent controls
* replace the CLI product with the headless runner (Phase 1 of remote-runner) ([#111](https://github.com/alfin-efendy/ryuzi/issues/111))
* native-only — remove the runtime concept ([#105](https://github.com/alfin-efendy/ryuzi/issues/105))

### Features

* add agentic session ownership and delegation ([#133](https://github.com/alfin-efendy/ryuzi/issues/133)) ([435c935](https://github.com/alfin-efendy/ryuzi/commit/435c935d9149f1ff19b956f9c1ac1968ead8802a))
* add session task artifacts ([3aa8b8f](https://github.com/alfin-efendy/ryuzi/commit/3aa8b8f49a192ca82d0a0431732c1c8d51d3a5ff))
* **agents:** add structured agent profiles ([03fc660](https://github.com/alfin-efendy/ryuzi/commit/03fc660f27e2fa783258ca164f5fd477d74fc870))
* **agents:** add structured agent profiles ([57a3976](https://github.com/alfin-efendy/ryuzi/commit/57a397615504a9557c9b1735ffddd0effdd0aef7))
* **automations:** add Automations hub with Scheduler, Hooks, and Commands tabs ([#131](https://github.com/alfin-efendy/ryuzi/issues/131)) ([b4d0f50](https://github.com/alfin-efendy/ryuzi/commit/b4d0f50f5bc71496cd95439cdcbb84169a8ce72a))
* **cockpit:** add install/uninstall/list installed-provider Tauri commands ([2e83939](https://github.com/alfin-efendy/ryuzi/commit/2e83939b20e68b34eae4312a5b6cf13ef6ef99a3))
* **cockpit:** chat & UI enhancement batch ([#95](https://github.com/alfin-efendy/ryuzi/issues/95)) ([510572b](https://github.com/alfin-efendy/ryuzi/commit/510572ba17e93e3b37c90ead3edaeb026b1c8a54))
* **cockpit:** chat enhancement batch 3 — floating plan panel, auto-continue, per-session permissions, attachment fixes, scroll polish ([#100](https://github.com/alfin-efendy/ryuzi/issues/100)) ([ab18930](https://github.com/alfin-efendy/ryuzi/commit/ab18930dc408e1040f1eaacca069a06397cffe58))
* **cockpit:** custom-provider RPCs and Tauri commands ([4129676](https://github.com/alfin-efendy/ryuzi/commit/412967684a19a4cc49a299780719e753c2331a09))
* **cockpit:** enhance model and account management ([#110](https://github.com/alfin-efendy/ryuzi/issues/110)) ([54c19dd](https://github.com/alfin-efendy/ryuzi/commit/54c19dd6de8af839913e8df972e30caaa771cc1f))
* **cockpit:** expose artifact commands ([93ae657](https://github.com/alfin-efendy/ryuzi/commit/93ae6570385678698b000f405344d4906a318a6f))
* **cockpit:** expose rooted child run events ([f38d237](https://github.com/alfin-efendy/ryuzi/commit/f38d237d55a96e785d6eb5adbb65a15c82e8e93e))
* **cockpit:** persist session archive state ([8f435a5](https://github.com/alfin-efendy/ryuzi/commit/8f435a5633c2a60040dc5d468ca7a5e10eb636d1))
* **cockpit:** Phase 4 session mgmt — notifications, message queue, pin-reorder, sub-agent roster ([#103](https://github.com/alfin-efendy/ryuzi/issues/103)) ([06b3246](https://github.com/alfin-efendy/ryuzi/commit/06b32462b63b63cfc9bafc03a4a3b8fca9d9eb68))
* **cockpit:** plugin install wizard — seamless OAuth (RFC 8414 + DCR + loopback callback), registry removal, provenance-gated skill packs ([#92](https://github.com/alfin-efendy/ryuzi/issues/92)) ([c2396c2](https://github.com/alfin-efendy/ryuzi/commit/c2396c2a0ae6ce541a933d830ab08d4433c2b993))
* **cockpit:** render linked agent dispatch cards ([ec6dfe6](https://github.com/alfin-efendy/ryuzi/commit/ec6dfe60291ccda2611d8a9d2154502a02a11e5d))
* **cockpit:** return typed workspace search entries ([9c9445e](https://github.com/alfin-efendy/ryuzi/commit/9c9445e1467e250b5016b17617eae7d6e57853dc))
* **cockpit:** unified plugins catalog — two-tab Installed | Browse with providers, Discord & skills ([#96](https://github.com/alfin-efendy/ryuzi/issues/96)) ([a140c2d](https://github.com/alfin-efendy/ryuzi/commit/a140c2d048f24e0312c2453325a4b0db2b689fee))
* **cockpit:** unify at-command context picker ([08968c3](https://github.com/alfin-efendy/ryuzi/commit/08968c38568534e058f580c3297018950a1a612c))
* configure route target effort ([#127](https://github.com/alfin-efendy/ryuzi/issues/127)) ([8835e89](https://github.com/alfin-efendy/ryuzi/commit/8835e89df81a1ee8d1eb9a98ebb3237fdb87c838))
* **core+cockpit:** Phase 1 — daemon as single engine host + control API, Cockpit thin client ([#98](https://github.com/alfin-efendy/ryuzi/issues/98)) ([d82b7e4](https://github.com/alfin-efendy/ryuzi/commit/d82b7e425e735749c810dfa694cf41ad5712ba6d))
* **core:** remote plugin catalog — signed feed, version-gated override, blocked denylist ([#113](https://github.com/alfin-efendy/ryuzi/issues/113)) ([7535362](https://github.com/alfin-efendy/ryuzi/commit/7535362b430fed728d3731f2e22309dddec5cbda))
* enhance agent management and per-agent learning ([#122](https://github.com/alfin-efendy/ryuzi/issues/122)) ([b128241](https://github.com/alfin-efendy/ryuzi/commit/b12824165a82e8ffb8dc062d1e972b9d9e6c6fbb))
* Hermes-parity Phases 4–6 — self-learning, group-chat orchestration, app-control ([#119](https://github.com/alfin-efendy/ryuzi/issues/119)) ([84b503f](https://github.com/alfin-efendy/ryuzi/commit/84b503f067f2874cfcfc9d7cef8c551cd5dfe18d))
* **models:** free-first Models overhaul — install gating, subscriptions, custom providers, usage-chart fix ([b9a0910](https://github.com/alfin-efendy/ryuzi/commit/b9a0910078b38fa9fd531eaff8e230657673242b))
* multi-mode approvals, don't-ask-again scopes, and cross-session Inbox ([#94](https://github.com/alfin-efendy/ryuzi/issues/94)) ([18e4c92](https://github.com/alfin-efendy/ryuzi/commit/18e4c924db58fa992169df2640e7b48178d0877e))
* native-only — remove the runtime concept ([#105](https://github.com/alfin-efendy/ryuzi/issues/105)) ([2e83415](https://github.com/alfin-efendy/ryuzi/commit/2e834152a6050cac1b49753928642c998ac8cbe4))
* plugin distribution hardening (install ledger, atomic install, trust gate, doctor) ([#104](https://github.com/alfin-efendy/ryuzi/issues/104)) ([566ece6](https://github.com/alfin-efendy/ryuzi/commit/566ece63efa69512bdd551e03bfed30b447a3e17))
* plugin extension surface (Track C) + extension runtime / code plugins (Track D) ([#116](https://github.com/alfin-efendy/ryuzi/issues/116)) ([8d90c8b](https://github.com/alfin-efendy/ryuzi/commit/8d90c8b47e38f3138fe0d7ea52acd154228d5485))
* remote runner — TLS pairing, Cockpit multi-runner, remote UX (Phases 2–4) ([#117](https://github.com/alfin-efendy/ryuzi/issues/117)) ([e59a2e7](https://github.com/alfin-efendy/ryuzi/commit/e59a2e73fe0d642cfa1bd4a3bf61fa242339c8bb))
* replace the CLI product with the headless runner (Phase 1 of remote-runner) ([#111](https://github.com/alfin-efendy/ryuzi/issues/111)) ([d100b78](https://github.com/alfin-efendy/ryuzi/commit/d100b785e0a7749f1f0cefcf7cf677bbc4839c83))


### Bug Fixes

* **daemon:** ensure state directory creation for fresh installs to prevent NotFound errors ([364f6b9](https://github.com/alfin-efendy/ryuzi/commit/364f6b9767490d7a7b94fe65c5173f110b038ed9))
* durable session queue and Cockpit session UX ([#128](https://github.com/alfin-efendy/ryuzi/issues/128)) ([0670146](https://github.com/alfin-efendy/ryuzi/commit/06701468b1634697b59fdb4ac3d41b3addd25b2f))
* **models:** probe unification (Kiro/MiMo/OpenAI), hide-invalid in all pickers, shared ModelPicker ([#86](https://github.com/alfin-efendy/ryuzi/issues/86)) ([05ac5d5](https://github.com/alfin-efendy/ryuzi/commit/05ac5d5fae8902ffa8d56b1f5337b168ec12be05))


### Code Refactoring

* **cockpit:** remove legacy agent controls ([f523591](https://github.com/alfin-efendy/ryuzi/commit/f523591b2403327539a965ee5c35daab296337de))

## [Unreleased]


### Features

* **agents:** Agents hub and per-agent detail screens (model/permissions/skills-tools/advanced/Learning tabs), primary-agent selection on New session, `@mention` composer delegation, child-run transcripts with Active/Done right-panel navigation, and per-agent model assignment (concrete model + effort, or a named route)


### Removed

* global Learning sidebar/panel, Settings screen agent-default controls, composer model/effort/permission-mode pickers, and the Orchestrate toggle/task strip — superseded by per-agent Learning tabs and `@mention`/`delegate_agent`-based delegation

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
