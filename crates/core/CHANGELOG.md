# Changelog

## [0.4.0](https://github.com/alfin-efendy/ryuzi/compare/core-v0.3.0...core-v0.4.0) (2026-07-24)


### Features

* Phase 6 — GitHub connector, WASM WebSocket capability + Discord gateway migration, Atlassian/Bitbucket connectors ([#165](https://github.com/alfin-efendy/ryuzi/issues/165)) ([e9caca2](https://github.com/alfin-efendy/ryuzi/commit/e9caca2dcc9b8cd8f0a91ded6d8b42e9595a1944))
* Phase 7 — WASM provider migration (transitional) + revocation/doctor/docs/release hardening ([#168](https://github.com/alfin-efendy/ryuzi/issues/168)) ([b3b73e2](https://github.com/alfin-efendy/ryuzi/commit/b3b73e2044433408edb1450b9ec69e99d2dcacb0))
* **plugins:** component catalog migration + end-to-end OAuth connect ([#169](https://github.com/alfin-efendy/ryuzi/issues/169)) ([ba6612a](https://github.com/alfin-efendy/ryuzi/commit/ba6612a93f356e6b8be5cbf2221415954603e55c))
* WASM Component Plugin Platform — signed bundles, generic adapters, bootstrap providers & Cockpit UX (Phases 1–5) ([#163](https://github.com/alfin-efendy/ryuzi/issues/163)) ([4993ae1](https://github.com/alfin-efendy/ryuzi/commit/4993ae1b81a488490669abf2c72d2b2625ce3313))


### Bug Fixes

* **ci:** stop macOS cargo test timeout and AgentActionsMenu flake ([#167](https://github.com/alfin-efendy/ryuzi/issues/167)) ([55fb165](https://github.com/alfin-efendy/ryuzi/commit/55fb1655cbd074542fa1af2edec68ea60d5e6a8b))

## [0.3.0](https://github.com/alfin-efendy/ryuzi/compare/core-v0.2.0...core-v0.3.0) (2026-07-18)


### ⚠ BREAKING CHANGES

* **core:** delete legacy agent runtime paths
* **core:** remove legacy agent storage on upgrade
* replace the CLI product with the headless runner (Phase 1 of remote-runner) ([#111](https://github.com/alfin-efendy/ryuzi/issues/111))
* native-only — remove the runtime concept ([#105](https://github.com/alfin-efendy/ryuzi/issues/105))
* **release:** self-updaters in ryuzi <= 0.6.0 look for the old -unknown- asset names, will find no matching asset in this and future releases, and silently stay on their current version. Reinstall once via `curl -fsSL https://raw.githubusercontent.com/alfin-efendy/ryuzi/main/install.sh | sh` or `npm i -g ryuzi` to pick up the new naming scheme.

### Features

* add agentic session ownership and delegation ([#133](https://github.com/alfin-efendy/ryuzi/issues/133)) ([435c935](https://github.com/alfin-efendy/ryuzi/commit/435c935d9149f1ff19b956f9c1ac1968ead8802a))
* add session task artifacts ([3aa8b8f](https://github.com/alfin-efendy/ryuzi/commit/3aa8b8f49a192ca82d0a0431732c1c8d51d3a5ff))
* **agent:** add artifact tools ([3daff6b](https://github.com/alfin-efendy/ryuzi/commit/3daff6b7c5c1f4c92ee6875b57540ba432db1967))
* **agents:** add structured agent profiles ([03fc660](https://github.com/alfin-efendy/ryuzi/commit/03fc660f27e2fa783258ca164f5fd477d74fc870))
* **agents:** add structured agent profiles ([57a3976](https://github.com/alfin-efendy/ryuzi/commit/57a397615504a9557c9b1735ffddd0effdd0aef7))
* **api:** fetch session artifacts ([87db857](https://github.com/alfin-efendy/ryuzi/commit/87db857ab66993361c3a0caa86292d6e9761b2fd))
* **api:** list session artifacts ([afa1a28](https://github.com/alfin-efendy/ryuzi/commit/afa1a2848168beb6ce606a86d186442087082437))
* **api:** run artifact retention ([c373af0](https://github.com/alfin-efendy/ryuzi/commit/c373af09decdd69018476b41f8c3fc27c55a8165))
* **automations:** add Automations hub with Scheduler, Hooks, and Commands tabs ([#131](https://github.com/alfin-efendy/ryuzi/issues/131)) ([b4d0f50](https://github.com/alfin-efendy/ryuzi/commit/b4d0f50f5bc71496cd95439cdcbb84169a8ce72a))
* **cockpit:** chat & UI enhancement batch ([#95](https://github.com/alfin-efendy/ryuzi/issues/95)) ([510572b](https://github.com/alfin-efendy/ryuzi/commit/510572ba17e93e3b37c90ead3edaeb026b1c8a54))
* **cockpit:** chat enhancement batch 3 — floating plan panel, auto-continue, per-session permissions, attachment fixes, scroll polish ([#100](https://github.com/alfin-efendy/ryuzi/issues/100)) ([ab18930](https://github.com/alfin-efendy/ryuzi/commit/ab18930dc408e1040f1eaacca069a06397cffe58))
* **cockpit:** custom-provider RPCs and Tauri commands ([4129676](https://github.com/alfin-efendy/ryuzi/commit/412967684a19a4cc49a299780719e753c2331a09))
* **cockpit:** enhance model and account management ([#110](https://github.com/alfin-efendy/ryuzi/issues/110)) ([54c19dd](https://github.com/alfin-efendy/ryuzi/commit/54c19dd6de8af839913e8df972e30caaa771cc1f))
* **cockpit:** expose rooted child run events ([f38d237](https://github.com/alfin-efendy/ryuzi/commit/f38d237d55a96e785d6eb5adbb65a15c82e8e93e))
* **cockpit:** persist session archive state ([8f435a5](https://github.com/alfin-efendy/ryuzi/commit/8f435a5633c2a60040dc5d468ca7a5e10eb636d1))
* **cockpit:** plugin install wizard — seamless OAuth (RFC 8414 + DCR + loopback callback), registry removal, provenance-gated skill packs ([#92](https://github.com/alfin-efendy/ryuzi/issues/92)) ([c2396c2](https://github.com/alfin-efendy/ryuzi/commit/c2396c2a0ae6ce541a933d830ab08d4433c2b993))
* **cockpit:** render linked agent dispatch cards ([ec6dfe6](https://github.com/alfin-efendy/ryuzi/commit/ec6dfe60291ccda2611d8a9d2154502a02a11e5d))
* **cockpit:** return typed workspace search entries ([9c9445e](https://github.com/alfin-efendy/ryuzi/commit/9c9445e1467e250b5016b17617eae7d6e57853dc))
* **cockpit:** unified plugins catalog — two-tab Installed | Browse with providers, Discord & skills ([#96](https://github.com/alfin-efendy/ryuzi/issues/96)) ([a140c2d](https://github.com/alfin-efendy/ryuzi/commit/a140c2d048f24e0312c2453325a4b0db2b689fee))
* **cockpit:** unify at-command context picker ([08968c3](https://github.com/alfin-efendy/ryuzi/commit/08968c38568534e058f580c3297018950a1a612c))
* configure route target effort ([#127](https://github.com/alfin-efendy/ryuzi/issues/127)) ([8835e89](https://github.com/alfin-efendy/ryuzi/commit/8835e89df81a1ee8d1eb9a98ebb3237fdb87c838))
* **core+cockpit:** configurable worktree directory setting ([#109](https://github.com/alfin-efendy/ryuzi/issues/109)) ([aca14ae](https://github.com/alfin-efendy/ryuzi/commit/aca14aebe3d376bc846b2a3cdf9714c01c225baa))
* **core+cockpit:** Phase 1 — daemon as single engine host + control API, Cockpit thin client ([#98](https://github.com/alfin-efendy/ryuzi/issues/98)) ([d82b7e4](https://github.com/alfin-efendy/ryuzi/commit/d82b7e425e735749c810dfa694cf41ad5712ba6d))
* **core:** add actionable edit diagnostics ([fca9069](https://github.com/alfin-efendy/ryuzi/commit/fca906905fb9f853cf760b953b443491506e426a))
* **core:** add artifact retention store queries ([fbfea6d](https://github.com/alfin-efendy/ryuzi/commit/fbfea6dfa04b3995a92c308616086a7e5fb6060f))
* **core:** add declarative native tool contracts ([3a198f1](https://github.com/alfin-efendy/ryuzi/commit/3a198f137f0d4b223398b5d00e92e9a33dab1304))
* **core:** add MiMo Token Plan and OpenCode Go subscription providers ([dac3922](https://github.com/alfin-efendy/ryuzi/commit/dac392254d65de23b5bcffe611a148db4a4e3d10))
* **core:** add native tool capability resolution ([2f6c523](https://github.com/alfin-efendy/ryuzi/commit/2f6c5232cfb634489234525046505f190fbd192c))
* **core:** add provider-agnostic native tools v2 ([e27c7b9](https://github.com/alfin-efendy/ryuzi/commit/e27c7b927c33b9452c467701a0fb67d082e4c63b))
* **core:** add run-scoped AgentRunContextUsage event ([93f6a36](https://github.com/alfin-efendy/ryuzi/commit/93f6a367383ad06bf3183196b58195a2cec422d5))
* **core:** add structured native tool results ([7df8d7a](https://github.com/alfin-efendy/ryuzi/commit/7df8d7a8075b91cbf1eb9505a01fb29a76089c30))
* **core:** auto-seed MiMo and OpenCode free connections on first run ([6f03f5b](https://github.com/alfin-efendy/ryuzi/commit/6f03f5b862090a01efbeae3e1634bcfb213ca6e8))
* **core:** build the free route from probed MiMo/OpenCode models in the background ([e6bec11](https://github.com/alfin-efendy/ryuzi/commit/e6bec11078eb4cbaba5c6d0d144e0604d5d89ccd))
* **core:** create artifacts from session attachments ([fad1308](https://github.com/alfin-efendy/ryuzi/commit/fad13085859f3c9da2e99405b2e5d8b71d37199a))
* **core:** dynamic user-defined custom providers resolved via the descriptor cache ([d54f20d](https://github.com/alfin-efendy/ryuzi/commit/d54f20d10de6fc9c96ed2fa843b03f3eeb55044c))
* **core:** expose install/uninstall provider RPCs and gate plugin install-state on the set ([0942b4d](https://github.com/alfin-efendy/ryuzi/commit/0942b4d6f35fc672d150b233cc34d40963dbe010))
* **core:** freeze native tool plans ([8d010f1](https://github.com/alfin-efendy/ryuzi/commit/8d010f1394e8c6e0ccb2f2493ae7b593f4e12fa2))
* **core:** freeze one V2 tool facade per run ([8745c54](https://github.com/alfin-efendy/ryuzi/commit/8745c544941f4d334837225175a43dfe038375cf))
* **core:** link delegated runs to tool calls ([2877574](https://github.com/alfin-efendy/ryuzi/commit/2877574770979735fddb003eb3c47eeeb6bd0084))
* **core:** manage artifact archive access ([4f683bd](https://github.com/alfin-efendy/ryuzi/commit/4f683bde3b8bd6c4603a831a0b9abdf5ce18bdaf))
* **core:** normalize native file references ([106d062](https://github.com/alfin-efendy/ryuzi/commit/106d0623069a8378a161b87d1242d04ae96fbcb7))
* **core:** persist agent dispatch linkage ([0ae61ea](https://github.com/alfin-efendy/ryuzi/commit/0ae61eae789c4393a3b647b2f16770d31e7a69d4))
* **core:** persist an installed-providers set with default seed ([da81c4d](https://github.com/alfin-efendy/ryuzi/commit/da81c4db345438901c0841aff730d168978632d3))
* **core:** persist per-run context usage on agent_runs (migration 43) ([da5a151](https://github.com/alfin-efendy/ryuzi/commit/da5a151ca9710c66b05fb554e19853d354fdb6bf))
* **core:** persist session archive state ([6bfaff8](https://github.com/alfin-efendy/ryuzi/commit/6bfaff8f95975781f09248e0bd0b084978ee9fd0))
* **core:** persist task artifact metadata ([dc9b332](https://github.com/alfin-efendy/ryuzi/commit/dc9b3324a5a2988459212f1d444d907a6d8f5e5a))
* **core:** Phase 1 — init context size & per-turn latency ([#130](https://github.com/alfin-efendy/ryuzi/issues/130)) ([65f580f](https://github.com/alfin-efendy/ryuzi/commit/65f580fb5b6df4cc9385983f69e008cf1a4bc56b))
* **core:** Phase 2 — lazy / deferred tool surface ([#132](https://github.com/alfin-efendy/ryuzi/issues/132)) ([913d0f8](https://github.com/alfin-efendy/ryuzi/commit/913d0f8ca6aa00deac3f75f71494312f5614a9be))
* **core:** preflight native file targets ([0b77031](https://github.com/alfin-efendy/ryuzi/commit/0b770314488cef2717c91dcceb1c15c95ab2f09d))
* **core:** remote plugin catalog — signed feed, version-gated override, blocked denylist ([#113](https://github.com/alfin-efendy/ryuzi/issues/113)) ([7535362](https://github.com/alfin-efendy/ryuzi/commit/7535362b430fed728d3731f2e22309dddec5cbda))
* **core:** remove legacy agent storage on upgrade ([d6158e1](https://github.com/alfin-efendy/ryuzi/commit/d6158e1d12ccad00090e69fd608efedb105a2d0f))
* **core:** share and retain session artifacts ([83ae9b6](https://github.com/alfin-efendy/ryuzi/commit/83ae9b6459bf996a08fa027140688ab491031ab7))
* **core:** split the V2 memory facade ([d0f279d](https://github.com/alfin-efendy/ryuzi/commit/d0f279d92f1d7d1a4f8debf087c55a02b4eab946))
* **core:** store artifact payloads safely ([5b128b9](https://github.com/alfin-efendy/ryuzi/commit/5b128b943becfe08802f0ddf9a7b4729f2dd2604))
* **core:** validate frozen native tool arguments ([a4496fb](https://github.com/alfin-efendy/ryuzi/commit/a4496fb4e4de3d7437b9df61b87c1a5b4b2fa6cc))
* cost & context visibility — model price table, per-session spend, context ring ([#99](https://github.com/alfin-efendy/ryuzi/issues/99)) ([9f45b2b](https://github.com/alfin-efendy/ryuzi/commit/9f45b2bf60618dc8616ec763d3b47a650a9b070a))
* enhance agent management and per-agent learning ([#122](https://github.com/alfin-efendy/ryuzi/issues/122)) ([b128241](https://github.com/alfin-efendy/ryuzi/commit/b12824165a82e8ffb8dc062d1e972b9d9e6c6fbb))
* Hermes-parity Phases 4–6 — self-learning, group-chat orchestration, app-control ([#119](https://github.com/alfin-efendy/ryuzi/issues/119)) ([84b503f](https://github.com/alfin-efendy/ryuzi/commit/84b503f067f2874cfcfc9d7cef8c551cd5dfe18d))
* **models:** free-first Models overhaul — install gating, subscriptions, custom providers, usage-chart fix ([b9a0910](https://github.com/alfin-efendy/ryuzi/commit/b9a0910078b38fa9fd531eaff8e230657673242b))
* multi-mode approvals, don't-ask-again scopes, and cross-session Inbox ([#94](https://github.com/alfin-efendy/ryuzi/issues/94)) ([18e4c92](https://github.com/alfin-efendy/ryuzi/commit/18e4c924db58fa992169df2640e7b48178d0877e))
* native-only — remove the runtime concept ([#105](https://github.com/alfin-efendy/ryuzi/issues/105)) ([2e83415](https://github.com/alfin-efendy/ryuzi/commit/2e834152a6050cac1b49753928642c998ac8cbe4))
* **native:** context window management — token accounting, model metadata, durable compaction, prompt caching ([#89](https://github.com/alfin-efendy/ryuzi/issues/89)) ([9d7ef98](https://github.com/alfin-efendy/ryuzi/commit/9d7ef981f7a6890c05b2211d6dc31db71217c11f))
* **native:** emit run-scoped context usage for child (ToolsOnly) loops ([d819dc0](https://github.com/alfin-efendy/ryuzi/commit/d819dc03900386093c23731952038a2a287e77cd))
* plugin distribution hardening (install ledger, atomic install, trust gate, doctor) ([#104](https://github.com/alfin-efendy/ryuzi/issues/104)) ([566ece6](https://github.com/alfin-efendy/ryuzi/commit/566ece63efa69512bdd551e03bfed30b447a3e17))
* plugin extension surface (Track C) + extension runtime / code plugins (Track D) ([#116](https://github.com/alfin-efendy/ryuzi/issues/116)) ([8d90c8b](https://github.com/alfin-efendy/ryuzi/commit/8d90c8b47e38f3138fe0d7ea52acd154228d5485))
* **release:** drop -unknown from linux asset names ([#81](https://github.com/alfin-efendy/ryuzi/issues/81)) ([9fb26bf](https://github.com/alfin-efendy/ryuzi/commit/9fb26bf98fc913a90f6266f0b3923c4b43a4dd12))
* remote runner — TLS pairing, Cockpit multi-runner, remote UX (Phases 2–4) ([#117](https://github.com/alfin-efendy/ryuzi/issues/117)) ([e59a2e7](https://github.com/alfin-efendy/ryuzi/commit/e59a2e73fe0d642cfa1bd4a3bf61fa242339c8bb))
* replace the CLI product with the headless runner (Phase 1 of remote-runner) ([#111](https://github.com/alfin-efendy/ryuzi/issues/111)) ([d100b78](https://github.com/alfin-efendy/ryuzi/commit/d100b785e0a7749f1f0cefcf7cf677bbc4839c83))
* **store:** add promote_if_idle for atomic Idle-&gt;Running session status ([32fe334](https://github.com/alfin-efendy/ryuzi/commit/32fe334e838ffd54ccdac4e97b90b57d1d9b2f5b))


### Bug Fixes

* **agents:** align control tests with the broadened primary validation ([6cffa64](https://github.com/alfin-efendy/ryuzi/commit/6cffa64bc4ac846e77c920e51fb2eb8c67a708ce))
* **agents:** green CI for structured agent profiles ([7e9bb42](https://github.com/alfin-efendy/ryuzi/commit/7e9bb4213995fda62f466bf6330d1fd33ff1fb76))
* **bindings:** preserve optional session event fields ([82aed68](https://github.com/alfin-efendy/ryuzi/commit/82aed6874c354dcc8f64ffb1ccd96368058156c7))
* **ci:** satisfy biome and clippy ([56da4a9](https://github.com/alfin-efendy/ryuzi/commit/56da4a91f3b8cfcbedc9e8ad49434dee28981b22))
* **cockpit:** chat batch 2 — stuck status, failover rotation, tool_use 400, mid-chat model switch, todo bar, tool cards, composer drafts/history/resize ([#90](https://github.com/alfin-efendy/ryuzi/issues/90)) ([8fa4ad2](https://github.com/alfin-efendy/ryuzi/commit/8fa4ad285f7df3cbafb3d25914c5c58c4c293963))
* **cockpit:** per-run context ring for sub-agents ([e86c801](https://github.com/alfin-efendy/ryuzi/commit/e86c801c7f40b4caaa7cd2eeb13ada4ee9042ecf))
* **cockpit:** preserve dispatch ownership and errors ([3159e11](https://github.com/alfin-efendy/ryuzi/commit/3159e11fe2c347cf9d41866ce1dea656997ac694))
* **cockpit:** restore route model metadata ([1f2175e](https://github.com/alfin-efendy/ryuzi/commit/1f2175e751472befcc3748611c3c98101f575c53))
* **cockpit:** restore route model metadata ([ef5a920](https://github.com/alfin-efendy/ryuzi/commit/ef5a9205d8f644bb1c7dddebf49f02b5efc1ca3c))
* complete artifact main integration ([5d641cd](https://github.com/alfin-efendy/ryuzi/commit/5d641cdb7244616c4111ef0b236ec6520b7648b5))
* complete custom-provider removal (rows + UI) ([86cc21f](https://github.com/alfin-efendy/ryuzi/commit/86cc21fbf23b6b009d483d6d2eae2916bb23da53))
* **context:** recover from context-window overflow (compaction doom-loop) ([abf7502](https://github.com/alfin-efendy/ryuzi/commit/abf7502db59235e95b9e5d2b8f96dcd89b1a069b))
* **context:** reserve output headroom in the auto-compact threshold ([2fc5368](https://github.com/alfin-efendy/ryuzi/commit/2fc536876d59c0d95fa76f1ea6cb37027208f4c9))
* **context:** sanitize tool pairing in the compaction request ([1b9474d](https://github.com/alfin-efendy/ryuzi/commit/1b9474dcfe4ed475f7b789b895b33f393c2c5865))
* **context:** stop cleanly when compaction fails over the hard window ([71c6697](https://github.com/alfin-efendy/ryuzi/commit/71c669767c0f482c18a0f2935449e608e120d057))
* **context:** strip tool_results from compacted history; review nits ([f7e0a45](https://github.com/alfin-efendy/ryuzi/commit/f7e0a45cd295b2720c0343752a2324a306ca26f1))
* **control:** promote session to Running on continue so it shows running and is stoppable ([f9e6b66](https://github.com/alfin-efendy/ryuzi/commit/f9e6b66f4c68eca02f8d361d5ce12296d86f91f4))
* **core:** align legacy tool policy lookup ([891ed86](https://github.com/alfin-efendy/ryuzi/commit/891ed863fab9d4a2eafa00e3a2f235e98aeb4963))
* **core:** align native capability routing ([0c6a4eb](https://github.com/alfin-efendy/ryuzi/commit/0c6a4eb398d151c0cbc0df9030e25b16b7a647c6))
* **core:** close edit diagnostics review gaps ([22fcc82](https://github.com/alfin-efendy/ryuzi/commit/22fcc82463a43d8d7d487b739467df96cc66d81c))
* **core:** close native tool result lifecycle gaps ([4cda5eb](https://github.com/alfin-efendy/ryuzi/commit/4cda5eb941f70fd122ce68486eba091994b0a169))
* **core:** close native tool review gaps ([9fde494](https://github.com/alfin-efendy/ryuzi/commit/9fde494ca1c1ce5c0b758651b79130e35f9d8311))
* **core:** close the free-route rebuild missing-route window ([2ccd332](https://github.com/alfin-efendy/ryuzi/commit/2ccd3329c4f995fd0b499cd537b554c850dc5a7f))
* **core:** count admitted V2 availability failures ([d473d65](https://github.com/alfin-efendy/ryuzi/commit/d473d6567dd3971f658b1e35a38148784c4a7415))
* **core:** enforce artifact storage quotas atomically ([878ab23](https://github.com/alfin-efendy/ryuzi/commit/878ab23464d7327c05cd6d8ed18c8d52ebcef9fa))
* **core:** finalize review runs and validate V2 events ([a3f45b9](https://github.com/alfin-efendy/ryuzi/commit/a3f45b9474fa9920e6fc4b4814f9a995ea800c3c))
* **core:** follow chained local tool refs ([3a0db86](https://github.com/alfin-efendy/ryuzi/commit/3a0db86bdd3895d8debf5c914eac20c216cbce9c))
* **core:** guard archived artifact creation ([23420de](https://github.com/alfin-efendy/ryuzi/commit/23420deff08fa2e3f2c88d888cc3592ec7d7caab))
* **core:** guard session queue mutations for read-only history ([eec0ab0](https://github.com/alfin-efendy/ryuzi/commit/eec0ab0a3b00516f628520ed08c6782e2332b239))
* **core:** harden file preflight boundaries ([ed6f18d](https://github.com/alfin-efendy/ryuzi/commit/ed6f18d7c6a13ba171b9e7e72f0e126ecfc6b201))
* **core:** harden frozen tool plan validation ([4857cb3](https://github.com/alfin-efendy/ryuzi/commit/4857cb3cca5491554ad6abd76f514a730b26d4bf))
* **core:** harden missing-path candidates ([19d4483](https://github.com/alfin-efendy/ryuzi/commit/19d448394ed8692b4e275470f1f4d6a667fb2812))
* **core:** harden native file target routing ([29f487f](https://github.com/alfin-efendy/ryuzi/commit/29f487f340288d938a20627913788fc2adb1dbc0))
* **core:** harden native tool contracts ([d973557](https://github.com/alfin-efendy/ryuzi/commit/d97355761b61d0047130d7fd9ab7e4725c9199bc))
* **core:** harden split memory facade ([1cff8df](https://github.com/alfin-efendy/ryuzi/commit/1cff8dffeace604c4c861c311e57438f43b22567))
* **core:** implement process liveness and termination on Windows ([#107](https://github.com/alfin-efendy/ryuzi/issues/107)) ([64983f6](https://github.com/alfin-efendy/ryuzi/commit/64983f6a0cfad50b8be6f919a1af0bdf85184963))
* **core:** keep historical agent sessions read only ([85a94fa](https://github.com/alfin-efendy/ryuzi/commit/85a94fab30bafa628e4764064a2d20c2292a3bca))
* **core:** load artifact settings at startup ([175b62c](https://github.com/alfin-efendy/ryuzi/commit/175b62c75c55a3efbee75f1414b2c7679fe0fd39))
* **core:** load legacy grouped tool plans ([a591f37](https://github.com/alfin-efendy/ryuzi/commit/a591f3782d5e8f5799bb251efb0881487535759c))
* **core:** make provider installed-state authoritative on the set only ([00e95c2](https://github.com/alfin-efendy/ryuzi/commit/00e95c2bbea064aadebb1a6a0907a10d29257e12))
* **core:** MiMo transient risk-control block → unknown, not persisted invalid ([#88](https://github.com/alfin-efendy/ryuzi/issues/88)) ([dfd26f1](https://github.com/alfin-efendy/ryuzi/commit/dfd26f1af2174a398fadd10612545e0751b4424c))
* **core:** normalize Anthropic OAuth tool schemas ([#118](https://github.com/alfin-efendy/ryuzi/issues/118)) ([849d245](https://github.com/alfin-efendy/ryuzi/commit/849d245d4a7019eb9c013df9667ef210cb6f03d5))
* **core:** pin normalized file targets ([10aefd2](https://github.com/alfin-efendy/ryuzi/commit/10aefd29ebad5b531a67bdd88210c37c4a689c6d))
* **core:** preserve native tool compatibility ([2069dd2](https://github.com/alfin-efendy/ryuzi/commit/2069dd2a82b2c6c355a82135dbba4d4983d9f1b5))
* **core:** replay archive migration tests ([1b7d50b](https://github.com/alfin-efendy/ryuzi/commit/1b7d50b39d9564c91b7b7bb8b26ad9fb06bf215e))
* **core:** restore migration replay invariants ([20b2081](https://github.com/alfin-efendy/ryuzi/commit/20b20811c7b6d7e5a2bd6a128c9f3ea4ac644d28))
* **core:** schedule artifact retention ([8196171](https://github.com/alfin-efendy/ryuzi/commit/819617164904c7946c1f621052718af652e30100))
* **core:** scope agent artifact reads ([0cf53ae](https://github.com/alfin-efendy/ryuzi/commit/0cf53aed81c326d0f294b83c434dae788654bcb3))
* **core:** seed default agent model routes ([#123](https://github.com/alfin-efendy/ryuzi/issues/123)) ([1cac39f](https://github.com/alfin-efendy/ryuzi/commit/1cac39fc29c56ac85e356ca09ef7daa8a6063439))
* **core:** stop background delegation cancel from deadlocking (6h CI hang) ([ef22cf0](https://github.com/alfin-efendy/ryuzi/commit/ef22cf0e9aef5cd6cbc9a0321541f2b29375d1af))
* **core:** stop background delegation cancel from deadlocking the rust CI (6h hang) ([395d259](https://github.com/alfin-efendy/ryuzi/commit/395d25907bf6a5cc1a58b9f48aa31ab9f8f86bbd))
* **core:** support CRLF native edits ([#126](https://github.com/alfin-efendy/ryuzi/issues/126)) ([3400776](https://github.com/alfin-efendy/ryuzi/commit/3400776d2c5d4132dfa408e053ea934b16ddf764))
* **core:** track retention and full downloads ([c1fdffc](https://github.com/alfin-efendy/ryuzi/commit/c1fdffcaa1b3f2a51913afb4d87076c99bad1911))
* **daemon:** ensure free route creation for fresh installs to avoid agent validation issues ([c141076](https://github.com/alfin-efendy/ryuzi/commit/c1410762673566f8195a4bc1af3ddab3eda34daf))
* **daemon:** ensure state directory creation for fresh installs to prevent NotFound errors ([364f6b9](https://github.com/alfin-efendy/ryuzi/commit/364f6b9767490d7a7b94fe65c5173f110b038ed9))
* durable session queue and Cockpit session UX ([#128](https://github.com/alfin-efendy/ryuzi/issues/128)) ([0670146](https://github.com/alfin-efendy/ryuzi/commit/06701468b1634697b59fdb4ac3d41b3addd25b2f))
* **models:** actual test-model failures for OpenAI, Kiro, and MiMo ([#87](https://github.com/alfin-efendy/ryuzi/issues/87)) ([402afef](https://github.com/alfin-efendy/ryuzi/commit/402afef001271c24e3d3b6b01d2e137dc8a30b7a))
* **models:** probe unification (Kiro/MiMo/OpenAI), hide-invalid in all pickers, shared ModelPicker ([#86](https://github.com/alfin-efendy/ryuzi/issues/86)) ([05ac5d5](https://github.com/alfin-efendy/ryuzi/commit/05ac5d5fae8902ffa8d56b1f5337b168ec12be05))
* resolve skill reads, task schema, and startup transcript order ([#114](https://github.com/alfin-efendy/ryuzi/issues/114)) ([d1a48ff](https://github.com/alfin-efendy/ryuzi/commit/d1a48ff2317156a41422eaa814f5ddca52badee8))
* **runner:** stop foreground task sub-agents on session stop via the turn cancel token ([f2f88b0](https://github.com/alfin-efendy/ryuzi/commit/f2f88b00f2dd28b2db0aa18a4e7b2d91c8b7ff54))
* session shows Running and is stoppable during task; merge clarify into askuserquestion ([859dbd0](https://github.com/alfin-efendy/ryuzi/commit/859dbd067824c59d4e02022ce9cf5614ca6297e4))
* stabilize macOS CI checks ([#124](https://github.com/alfin-efendy/ryuzi/issues/124)) ([a77eba8](https://github.com/alfin-efendy/ryuzi/commit/a77eba895aa8e83b75e223d8d701757e6cce9f94))
* **store:** rebuild plugin_oauth_clients from stale v1 NOT NULL shape (migration 19) ([#93](https://github.com/alfin-efendy/ryuzi/issues/93)) ([9a7fac1](https://github.com/alfin-efendy/ryuzi/commit/9a7fac17d6d18c43b7247737d11f40ed93262eda))


### Code Refactoring

* **core:** delete legacy agent runtime paths ([6b77488](https://github.com/alfin-efendy/ryuzi/commit/6b77488e5e055cc3f71344025d5dd79a40be9fba))

## [Unreleased]


### Features

* **agents:** YAML/OKF per-agent registry (`agents/index.yaml`, `agents/subagents.yaml`, `agents/<id>.yaml`, `agents/<id>/knowledge/`) — persistent main agents with their own portable, credential-free memory/skill/review/journey knowledge, and unified `@mention`/`delegate_agent` delegation across agents, replacing the single-agent settings surface and the app orchestrator


### Breaking Changes

* **agents:** the first launch of this agent schema destructively removes the previous global agent settings, freeform memory files, Learning/curator state, and orchestration DAG data, then creates one main agent named Ryuzi; pre-upgrade sessions surface as read-only "Legacy agent" history and are never assigned to Ryuzi

## [0.2.0](https://github.com/alfin-efendy/ryuzi/compare/core-v0.1.0...core-v0.2.0) (2026-07-08)


### Features

* **cockpit:** chat enhancement batch — media in chat, turn summaries + edit cards, branch popover, model groups, open-in ([#75](https://github.com/alfin-efendy/ryuzi/issues/75)) ([3cce67c](https://github.com/alfin-efendy/ryuzi/commit/3cce67cf8cbf3596057423ba9e4bea434fb82c6c))
* **cockpit:** UI polish batch — solid overlays, Route groups, file viewer View/Code, Windows attachments, model Test All, instant sessions, git-URL/non-git projects ([#78](https://github.com/alfin-efendy/ryuzi/issues/78)) ([0cc9077](https://github.com/alfin-efendy/ryuzi/commit/0cc90770f35dcd6bf818d37f1003157ca3925d1a))
* **cockpit:** Windows bash fix, Ryuzi-only sessions, real branch controls, Combobox migration ([#72](https://github.com/alfin-efendy/ryuzi/issues/72)) ([9eb076d](https://github.com/alfin-efendy/ryuzi/commit/9eb076d315c1cdf237febd87a69d9b0a917ed0eb))
* **models:** provider category badges, free/free-tier providers, OpenAI-OAuth model fix (Phase A) ([#74](https://github.com/alfin-efendy/ryuzi/issues/74)) ([e6d269c](https://github.com/alfin-efendy/ryuzi/commit/e6d269cef013ba6d93ad965df3b5575bad2b01ec))
* **models:** Qwen Code + GitHub Copilot device-grant providers (Phase B) ([#76](https://github.com/alfin-efendy/ryuzi/issues/76)) ([8067a63](https://github.com/alfin-efendy/ryuzi/commit/8067a63f1fd2e79376ce47f0b5dbe406b384bc01))
* provider families + per-model router targets + HTTP endpoint failover ([#70](https://github.com/alfin-efendy/ryuzi/issues/70)) ([5e37347](https://github.com/alfin-efendy/ryuzi/commit/5e373477a74cf469f5bf33325b2873c50a859231))

## 0.1.0 (2026-07-06)


### Features

* **cockpit:** readable chat transcript — markdown turns, thought & tool chips, live user bubble ([#30](https://github.com/alfin-efendy/ryuzi/issues/30)) ([a51e143](https://github.com/alfin-efendy/ryuzi/commit/a51e143e469957909c7875b4dfb851f4c688310e))
* **cockpit:** Rust engine (R0) + Tauri v2 desktop app (R1) ([#17](https://github.com/alfin-efendy/ryuzi/issues/17)) ([e3c441b](https://github.com/alfin-efendy/ryuzi/commit/e3c441b87605788d825a9da295fe767a88855fab))
* **core, cockpit:** durable transcript history + extensibility architecture (Spec 1 & 2) ([#26](https://github.com/alfin-efendy/ryuzi/issues/26)) ([561ae52](https://github.com/alfin-efendy/ryuzi/commit/561ae52f9273a297aa63e95ae947aff03d5834d7))
* Kiro free tier — device flow + IDE import + AWS event-stream ([#55](https://github.com/alfin-efendy/ryuzi/issues/55)) ([f49a6e5](https://github.com/alfin-efendy/ryuzi/commit/f49a6e5e88a3c4177c514a619538af6e492cc54b))
* models & providers overhaul — provider detail views, capability-aware routing, account round-robin ([#56](https://github.com/alfin-efendy/ryuzi/issues/56)) ([f49a6e5](https://github.com/alfin-efendy/ryuzi/commit/f49a6e5e88a3c4177c514a619538af6e492cc54b))
* Models & Runtime — embedded local LLM router (Phase 1) ([#32](https://github.com/alfin-efendy/ryuzi/issues/32)) ([6a5ca2b](https://github.com/alfin-efendy/ryuzi/commit/6a5ca2ba09db0b577e88a9a1e7ec4414d321dc0c))
* Models & Runtime F2a - Responses API, usage tracking, mid-stream error fix ([#47](https://github.com/alfin-efendy/ryuzi/issues/47)) ([d60308a](https://github.com/alfin-efendy/ryuzi/commit/d60308a11e22fadd772d59cb09cc4f77197d2fce))
* Models & Runtime F2b - OAuth (Claude sub + Codex) + OpenCode Free ([#51](https://github.com/alfin-efendy/ryuzi/issues/51)) ([4c47e9e](https://github.com/alfin-efendy/ryuzi/commit/4c47e9ebba4c6ef163a56e0f9d4f9352a135d160))
* native agent runtime — in-process harness, native tools, session export/import/share ([#54](https://github.com/alfin-efendy/ryuzi/issues/54)) ([f49a6e5](https://github.com/alfin-efendy/ryuzi/commit/f49a6e5e88a3c4177c514a619538af6e492cc54b))
* native runtime orchestration, parallel delegation & persistent memory ([#63](https://github.com/alfin-efendy/ryuzi/issues/63)) ([0cd9adc](https://github.com/alfin-efendy/ryuzi/commit/0cd9adc51d1f43cec2c625fd4222d01ae17d999c))
* OS-keychain credential encryption for router secrets ([#57](https://github.com/alfin-efendy/ryuzi/issues/57)) ([f49a6e5](https://github.com/alfin-efendy/ryuzi/commit/f49a6e5e88a3c4177c514a619538af6e492cc54b))
* plugin SDK — manifest contract v1, PluginHost, plugin-driven Cockpit menu, 24-integration catalog ([#58](https://github.com/alfin-efendy/ryuzi/issues/58)) ([f49a6e5](https://github.com/alfin-efendy/ryuzi/commit/f49a6e5e88a3c4177c514a619538af6e492cc54b))
* rewrite CLI to Rust on the cockpit stack (Spec 4A–4D-b) ([#28](https://github.com/alfin-efendy/ryuzi/issues/28)) ([a9231bd](https://github.com/alfin-efendy/ryuzi/commit/a9231bd2a865f816431869d7da3f80dbf2b0e7ac))
* Rust architecture alignment — docs, design-system adoption, god-module splits, guardrails, tests ([#50](https://github.com/alfin-efendy/ryuzi/issues/50)) ([f49a6e5](https://github.com/alfin-efendy/ryuzi/commit/f49a6e5e88a3c4177c514a619538af6e492cc54b))


### Bug Fixes

* **core:** acp testkit approval resolver survives preceding broadcast events ([#33](https://github.com/alfin-efendy/ryuzi/issues/33)) ([d872aad](https://github.com/alfin-efendy/ryuzi/commit/d872aad359a875c1be220ae8cc5356bcdaf54abe))
* **deps:** bump git2 to 0.21 — clears RUSTSEC-2026-0183/0184 ([#44](https://github.com/alfin-efendy/ryuzi/issues/44)) ([3287921](https://github.com/alfin-efendy/ryuzi/commit/32879211edeec051591227e8c065fc541605c1bd))
* native runtime not working on chat cockpit ([#61](https://github.com/alfin-efendy/ryuzi/issues/61)) ([e02888f](https://github.com/alfin-efendy/ryuzi/commit/e02888f630212b42c6e36ab0ba3c2f8262da5b38))
* **release:** one combined GitHub release for CLI + Cockpit; unblock goreleaser, cockpit bundling, npm publish ([f49a6e5](https://github.com/alfin-efendy/ryuzi/commit/f49a6e5e88a3c4177c514a619538af6e492cc54b))


### Miscellaneous Chores

* release 0.1.0 ([bec091d](https://github.com/alfin-efendy/ryuzi/commit/bec091d83bf544bed9e72db663e3664bf11e1e5b))
