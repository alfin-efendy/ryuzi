# Changelog

## [0.6.0](https://github.com/alfin-efendy/ryuzi/compare/v0.5.0...v0.6.0) (2026-07-08)


### Features

* **cockpit:** UI polish batch — solid overlays, Route groups, file viewer View/Code, Windows attachments, model Test All, instant sessions, git-URL/non-git projects ([#78](https://github.com/alfin-efendy/ryuzi/issues/78)) ([0cc9077](https://github.com/alfin-efendy/ryuzi/commit/0cc90770f35dcd6bf818d37f1003157ca3925d1a))
* **cockpit:** Windows bash fix, Ryuzi-only sessions, real branch controls, Combobox migration ([#72](https://github.com/alfin-efendy/ryuzi/issues/72)) ([9eb076d](https://github.com/alfin-efendy/ryuzi/commit/9eb076d315c1cdf237febd87a69d9b0a917ed0eb))

## [0.5.0](https://github.com/alfin-efendy/ryuzi/compare/v0.4.0...v0.5.0) (2026-07-06)


### Features

* Kiro free tier — device flow + IDE import + AWS event-stream ([#55](https://github.com/alfin-efendy/ryuzi/issues/55)) ([f49a6e5](https://github.com/alfin-efendy/ryuzi/commit/f49a6e5e88a3c4177c514a619538af6e492cc54b))
* models & providers overhaul — provider detail views, capability-aware routing, account round-robin ([#56](https://github.com/alfin-efendy/ryuzi/issues/56)) ([f49a6e5](https://github.com/alfin-efendy/ryuzi/commit/f49a6e5e88a3c4177c514a619538af6e492cc54b))
* native agent runtime — in-process harness, native tools, session export/import/share ([#54](https://github.com/alfin-efendy/ryuzi/issues/54)) ([f49a6e5](https://github.com/alfin-efendy/ryuzi/commit/f49a6e5e88a3c4177c514a619538af6e492cc54b))
* native runtime orchestration, parallel delegation & persistent memory ([#63](https://github.com/alfin-efendy/ryuzi/issues/63)) ([0cd9adc](https://github.com/alfin-efendy/ryuzi/commit/0cd9adc51d1f43cec2c625fd4222d01ae17d999c))
* OS-keychain credential encryption for router secrets ([#57](https://github.com/alfin-efendy/ryuzi/issues/57)) ([f49a6e5](https://github.com/alfin-efendy/ryuzi/commit/f49a6e5e88a3c4177c514a619538af6e492cc54b))
* plugin SDK — manifest contract v1, PluginHost, plugin-driven Cockpit menu, 24-integration catalog ([#58](https://github.com/alfin-efendy/ryuzi/issues/58)) ([f49a6e5](https://github.com/alfin-efendy/ryuzi/commit/f49a6e5e88a3c4177c514a619538af6e492cc54b))
* Rust architecture alignment — docs, design-system adoption, god-module splits, guardrails, tests ([#50](https://github.com/alfin-efendy/ryuzi/issues/50)) ([f49a6e5](https://github.com/alfin-efendy/ryuzi/commit/f49a6e5e88a3c4177c514a619538af6e492cc54b))


### Bug Fixes

* **release:** one combined GitHub release for CLI + Cockpit; unblock goreleaser, cockpit bundling, npm publish ([f49a6e5](https://github.com/alfin-efendy/ryuzi/commit/f49a6e5e88a3c4177c514a619538af6e492cc54b))

## [0.4.0](https://github.com/alfin-efendy/ryuzi/compare/v0.3.0...v0.4.0) (2026-07-04)


### Features

* rewrite CLI to Rust on the cockpit stack (Spec 4A–4D-b) ([#28](https://github.com/alfin-efendy/ryuzi/issues/28)) ([a9231bd](https://github.com/alfin-efendy/ryuzi/commit/a9231bd2a865f816431869d7da3f80dbf2b0e7ac))


### Miscellaneous Chores

* **cli:** pin next CLI release to 0.4.0 ([08c9aa6](https://github.com/alfin-efendy/ryuzi/commit/08c9aa6460ca82cba8f171c482d06f9dc466ee9c))
* release 0.1.0 ([bec091d](https://github.com/alfin-efendy/ryuzi/commit/bec091d83bf544bed9e72db663e3664bf11e1e5b))

## [0.3.0](https://github.com/alfin-efendy/ryuzi/compare/v0.2.0...v0.3.0) (2026-07-02)


### Features

* auto-update daemon (canary) + auto-resume running sessions ([#19](https://github.com/alfin-efendy/ryuzi/issues/19)) ([1ac0ad2](https://github.com/alfin-efendy/ryuzi/commit/1ac0ad23f41d63c50ee6a06c418662aa28e0878f))

## [0.2.0](https://github.com/alfin-efendy/harness-router/compare/v0.1.0...v0.2.0) (2026-06-29)


### Features

* **cli-ui:** TUI Pro visual enhancement for hr across all surfaces ([#8](https://github.com/alfin-efendy/harness-router/issues/8)) ([63fa565](https://github.com/alfin-efendy/harness-router/commit/63fa5659e748cbaa55cd268c104fb76f99aab519))
* **discord:** receive attachments from Discord and forward them to Claude ([#15](https://github.com/alfin-efendy/harness-router/issues/15)) ([f8ee210](https://github.com/alfin-efendy/harness-router/commit/f8ee210bbda73df6333e1d407747e85c7e78d0ef))

## 0.1.0 (2026-06-27)


### Features

* AppController daemon lifecycle, log buffer, sessions ([45e191e](https://github.com/alfin-efendy/herness-router/commit/45e191ebf1fc3edec7a448803b44552ed4a86d31))
* AppController settings + env surface ([477c86c](https://github.com/alfin-efendy/herness-router/commit/477c86c6d3088b8869c06078fd73065ac8dfcc03))
* assemble default provider catalog (discord + claude-code) ([6d86a39](https://github.com/alfin-efendy/herness-router/commit/6d86a39fbc65f4c1d7cf5b4af6e724d3a1fb40ed))
* brand assets + Harness Router wordmark/glyph in CLI ([790bc69](https://github.com/alfin-efendy/herness-router/commit/790bc690bb4034ceb86567093de3cdeeff3462b6))
* buildDaemon returns the control plane ([19a3b36](https://github.com/alfin-efendy/herness-router/commit/19a3b368a5f5ffe3a86e6364587bdd12811b9156))
* catalog-driven settings, migration, and daemon/cmdRun/controller wiring ([dc2a220](https://github.com/alfin-efendy/herness-router/commit/dc2a22031c072fd9b1d16ea7c324377e756c28d3))
* CI/CD multi-platform distribution (+ accumulated branch work) ([30911cd](https://github.com/alfin-efendy/herness-router/commit/30911cd3386678b216fb8a461a2eda993977fe5a))
* claude-code runtime descriptor ([000c3a3](https://github.com/alfin-efendy/herness-router/commit/000c3a3266a6c33d8eb903b04fcee5a9492cff47))
* conditional-required settings helper + global field metadata ([980af7d](https://github.com/alfin-efendy/herness-router/commit/980af7d8f4ea2eb558b2e1fedc5fd2548262a9e7))
* controller manages detached daemon via status file; quit no longer stops it ([3729aab](https://github.com/alfin-efendy/herness-router/commit/3729aabdeff8097236e5ae8c0086a113364a3685))
* daemon connecting state for honest startup feedback (review) ([8ae2d63](https://github.com/alfin-efendy/herness-router/commit/8ae2d63f775b5b4eae2957eaf6194d7dda01e40c))
* daemon status-file helpers (state file + pid-alive + state derivation) ([f021f3c](https://github.com/alfin-efendy/herness-router/commit/f021f3cb8fdb4942c66e40a06c585676365b2eac))
* descriptor-driven setup wizard (pick gateways/runtimes, detect status, field help) ([ce9350c](https://github.com/alfin-efendy/herness-router/commit/ce9350c181918895767e750c86db624778e948de))
* discord gateway descriptor ([4d7d1c7](https://github.com/alfin-efendy/herness-router/commit/4d7d1c7a1ee3b91cf3bf9d638dd917f716bf2cff))
* graceful gateway teardown (DiscordGateway.stop -&gt; client.destroy) ([6537938](https://github.com/alfin-efendy/herness-router/commit/6537938fc42149e7a6a3d1ef7ed9c19f8dd39339))
* grouped descriptor-driven Config tab with provider toggles ([b5e2930](https://github.com/alfin-efendy/herness-router/commit/b5e293088fc01f652d0168c23012381e583fb686))
* hidden __daemon entrypoint with graceful SIGTERM shutdown ([f9f92a8](https://github.com/alfin-efendy/herness-router/commit/f9f92a82cc8ec0189da02210906bcaedb1587066))
* hr dashboard app root with tab nav + options overlay ([36dbadd](https://github.com/alfin-efendy/herness-router/commit/36dbadd7fe00aaad0f5a4a80286cd1fe0c947011))
* hr dashboard tabs (status, daemon, sessions, config) ([6de91d8](https://github.com/alfin-efendy/herness-router/commit/6de91d84bbe0678bcabb5dc01d5b6e5a15668907))
* hr first-run setup wizard ([490f885](https://github.com/alfin-efendy/herness-router/commit/490f885c84ac970d21d919c7e146c1b8a47e8dcc))
* hr OPTIONS help text + version ([9861b29](https://github.com/alfin-efendy/herness-router/commit/9861b292db7bbeaa8d7c8182bfece4094a8e5378))
* hr ui theme, useController hook, shared components ([c94f3ac](https://github.com/alfin-efendy/herness-router/commit/c94f3ac050331e5f41f79909c6921d90b7af9b3e))
* **npm:** add binary-wrapper launcher with platform resolution + musl detection ([c315961](https://github.com/alfin-efendy/herness-router/commit/c315961c80afbdbefda1d683694512e487f9f0ec))
* provider descriptor types + catalog helper ([242231a](https://github.com/alfin-efendy/herness-router/commit/242231af2f45b5764b649cfd69bd8055a2024cf3))
* pure session-event reducer for hr dashboard ([4a7ad40](https://github.com/alfin-efendy/herness-router/commit/4a7ad40cfc139cb70fd969072bb870f89c6ee8bf))
* rename harness-&gt;hr, route bare cmd to ink dashboard, drop init/start commands ([3d3277b](https://github.com/alfin-efendy/herness-router/commit/3d3277ba54eb593a43d206d9bb9af30b4dea7959))
* reusable MultiSelectList component ([245940b](https://github.com/alfin-efendy/herness-router/commit/245940bdaa751cffe7c3a26a803a9a4d49e1d005))


### Bug Fixes

* biome-format npm launcher; build launcher test fixture in tempdir ([9076932](https://github.com/alfin-efendy/herness-router/commit/907693263e874f9e6f976c4149fe9b957a2d64f5))
* bounded daemon connect timeout; document pid-reuse limitation (review) ([a9034d7](https://github.com/alfin-efendy/herness-router/commit/a9034d741f937542030d8143d1c7e80ff4d24282))
* close parent log fd after detached spawn; assert spawn command head (review) ([7885cde](https://github.com/alfin-efendy/herness-router/commit/7885cdeb7fda755dab528a164a27f135424678a3))
* connectProject honors default_runtime; config clears stale default; restore coverage; wizard hint (final review) ([d56ce1b](https://github.com/alfin-efendy/herness-router/commit/d56ce1b967b2b207a873fe8b21fdd27ba48dd7ee))
* dedupe emitChange on daemon start/stop (review) ([dffb498](https://github.com/alfin-efendy/herness-router/commit/dffb498157d98754cdb8b92256a2d7717541731b))
* detection survives missing CLIs; CI git identity + linux/macOS matrix ([db597fa](https://github.com/alfin-efendy/herness-router/commit/db597fa65c3a9511254c871690a091e9b51d0315))
* guard daemon double-start, dedupe cmdRun SettingsStore, restore checkEnv test (review) ([1ca9d14](https://github.com/alfin-efendy/herness-router/commit/1ca9d14a447d87aea935e84915ab1f97825181f2))
* harden SessionsTab selection index against empty list (review) ([3a6ce4f](https://github.com/alfin-efendy/herness-router/commit/3a6ce4fe4c18605467728eb370ad7e5355209636))
* keep SettingsStore.set strict; seed required.test via raw db writes (review) ([a8f7dc1](https://github.com/alfin-efendy/herness-router/commit/a8f7dc1c32bb81ad6b47c5af2da0147a6c95e6a9))
* relaunch daemon correctly from compiled binary (no script path in $bunfs) ([b60a571](https://github.com/alfin-efendy/herness-router/commit/b60a571e9d5b388ce976792e066fae23db7c2abc))
* scope wizard orderFields to enabled providers; default_runtime by list order; drop dead import (review) ([f2e4ac1](https://github.com/alfin-efendy/herness-router/commit/f2e4ac15fccbb3e56e7147569d9402fe48063985))
* warn on missing HR_BINARY_PATH override; assert fixture build in launcher test ([3bf72e1](https://github.com/alfin-efendy/herness-router/commit/3bf72e1bf40290a74995a7fce375024d9e9e9916))


### Miscellaneous Chores

* release 0.1.0 ([bec091d](https://github.com/alfin-efendy/herness-router/commit/bec091d83bf544bed9e72db663e3664bf11e1e5b))
