# Route Target Effort Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Promote route-target effort to validated editable configuration, enforce route-owned effort precedence through execution, expose capability-driven target controls and summaries in Cockpit, and deterministically migrate supported OpenAI legacy suffixes.

**Architecture:** Keep `ModelRouteTarget.effort: Option<String>` in the existing route JSON contract, but remove the compatibility-only preservation path and validate every explicit value with Plan 1's shared `model_capabilities::resolve_for_model` resolver before persistence. Runtime policy treats named-route targets as authoritative over caller effort, then falls back to the global concrete-model preference and provider/model default; concrete-model calls retain their explicit caller effort. Cockpit derives each route target's choices from the same resolver-backed selectable-model metadata, while an idempotent post-schema route-data migration normalizes only provably valid OpenAI suffix targets.

**Tech Stack:** Rust 2021, Tokio, Serde/serde_json, SQLite through `Store`, Specta/tauri-specta generated TypeScript bindings, React 19, TypeScript, Zustand, `@ryuzi/ui` Combobox, Bun test, Testing Library, Playwright.

## Global Constraints

- Execute this plan only after Plan 1 has landed `crate::llm_router::model_capabilities::{resolve_for_model, ModelEffortCapabilities}` with `pub async fn resolve_for_model(store: &Store, key: &ModelPreferenceKey) -> anyhow::Result<ModelEffortCapabilities>` and `ModelEffortCapabilities::supports(&self, value: &str) -> bool`.
- `ModelRouteTarget` remains `{ provider: String, model: String, effort: Option<String> }`; `None` means **Model default**, not unsupported and not an empty string.
- Backend route validation must use Plan 1's shared resolver; do not duplicate provider/model capability tables or infer support from model names.
- Route-target precedence is exact and exhaustive: for a named route use explicit target effort, then the global preference keyed by that target's concrete `(provider family, model)`, then the resolver's provider/model default; ignore all project, session, agent, and per-request effort. For a direct concrete-model selection use the caller override, then that same global concrete-model preference, then provider/model default.
- If any candidate value at those precedence levels is not in Plan 1's resolved `supported` list, skip it and continue to the next level; unknown or explicit-empty capability metadata therefore produces no effort rather than guessing.
- Agent YAML that references a route contains no effort; this plan does not add effort to route references in agent or subagent configuration.
- Effort controls render only when the exact concrete model has a non-empty resolved `supported` list; no disabled placeholder is rendered.
- Route-target effort choices are exactly **Model default** plus resolver-supported values in resolver order.
- Changing a target model preserves its current effort only when the replacement model supports that exact value; otherwise it clears to `null`.
- Route cards show summaries only for explicit per-target overrides; targets using Model default retain the existing provider/model-only summary.
- Invalid stored route effort remains readable, but any save of that route is rejected until the target is corrected or reset to Model default.
- Legacy suffix migration is OpenAI-family-only and idempotent. It strips one terminal `-review`, then one recognized terminal effort suffix from `-minimal`, `-medium`, `-xhigh`, `-ultra`, `-high`, or `-low`, testing longest suffix first.
- Legacy migration changes a target only when its provider is exactly `openai`, its current `effort` is `None`, the stripped base model exists in the OpenAI registry/connection model inventory, and Plan 1 reports the parsed effort as supported for `openai/<base-model>`; otherwise it leaves the target byte-for-byte unchanged.
- The migration writes normalized route JSON before its completion marker. A crash between those writes safely replays the idempotent transform.
- Use `@ryuzi/ui` primitives; do not add raw `<button>`, `<input>`, `<textarea>`, or `<select>` elements.
- Regenerate `apps/cockpit/src/bindings.ts` with the repository alias `cargo gen-bindings`; never hand-edit it.
- Add no dependency and do not modify `Cargo.lock` or `bun.lock`.
- Preserve unrelated worktree changes. The current request is plan-only and must not create any commit; the commit commands below are for the later implementation session.

## File Structure

- Modify `crates/core/src/llm_router/routes.rs`: make route effort first-class, validate explicit target effort through Plan 1, expose one resolver-backed route-target capability DTO for Cockpit, and own the idempotent OpenAI route suffix migration.
- Modify `crates/core/src/llm_router/model_effort.rs`: rename compatibility-only route policy state to route-target policy state and encode target/global/provider precedence while ignoring caller effort for named routes.
- Modify `crates/core/src/llm_router/client.rs`: propagate configured target effort into concrete routed targets, remove request compatibility as route policy, and make route model summaries/defaults use target-first semantics.
- Modify `crates/core/src/api/connections_api.rs`: add the read-only `list_model_route_target_capabilities` RPC and keep route save errors as bad requests with the resolver's concrete target context.
- Modify `apps/cockpit/src-tauri/src/connections_cmd.rs`: add the thin capability-list proxy command.
- Modify `apps/cockpit/src-tauri/src/lib.rs`: register the new Tauri command for binding generation.
- Regenerate `apps/cockpit/src/bindings.ts`: emit `ModelRouteTargetCapability` and `commands.listModelRouteTargetCapabilities` from Rust.
- Modify `apps/cockpit/src/store-model-routes.ts`: hydrate routes and target capabilities together and refresh them after saves.
- Modify `apps/cockpit/src/views/ModelsView.tsx`: render conditional target effort pickers, preserve/clear effort on model changes, and summarize explicit overrides on route cards.
- Modify `apps/cockpit/src/views/ModelsView.test.tsx`: cover conditional controls, supported options, model-change preservation/clearing, save payload, and card summaries.
- Modify `apps/cockpit/e2e/mock-ipc.ts`: provide generated-command fixtures and persist route effort in the browser test transport.
- Modify `apps/cockpit/e2e/app.e2e.ts`: cover the route editor's resolver-driven effort control and saved card summary end to end.

---

### Task 1: First-Class Route Contract and Resolver-Backed Save Validation

**Files:**
- Modify: `crates/core/src/llm_router/routes.rs:19-31,191-234,351-388,390-483`

**Interfaces:**
- Consumes: Plan 1 `model_capabilities::resolve_for_model(store, &ModelPreferenceKey) -> anyhow::Result<ModelEffortCapabilities>` and `ModelEffortCapabilities::supports(&str) -> bool`.
- Produces: `pub struct ModelRouteTargetCapability { pub provider: String, pub model: String, pub supported: Vec<ReasoningEffortOption>, pub provider_default: Option<String> }`.
- Produces: `pub async fn list_model_route_target_capabilities(store: &Store) -> anyhow::Result<Vec<ModelRouteTargetCapability>>`.
- Produces internally: `async fn validate_target_effort(store: &Store, target: &ModelRouteTarget) -> anyhow::Result<()>`.
- Invariant: `save_model_route` persists the incoming sanitized `effort`; it never copies a prior target's effort over the submitted value.

- [ ] **Step 1: Write failing route-save and capability DTO tests**

In `crates/core/src/llm_router/routes.rs`, replace the compatibility-preservation test with these tests in the existing `#[cfg(test)] mod tests`. Reuse `mem_store()` and add the existing `connections::{ConnectionData, ConnectionRow}` test fixture pattern used by `model_capabilities` tests.

```rust
#[tokio::test]
async fn save_route_persists_explicit_effort_and_model_default() {
    let store = mem_store().await;
    let mut explicit = route("smart");
    explicit.targets[0] = ModelRouteTarget {
        provider: "anthropic".into(),
        model: "claude-opus-4-7".into(),
        effort: Some("max".into()),
    };

    let saved = save_model_route(&store, explicit).await.unwrap();
    assert_eq!(saved.targets[0].effort.as_deref(), Some("max"));

    let mut reset = saved;
    reset.targets[0].effort = None;
    let reset = save_model_route(&store, reset).await.unwrap();
    assert_eq!(reset.targets[0].effort, None);
    assert_eq!(list_model_routes(&store).await.unwrap()[0].targets[0].effort, None);
}

#[tokio::test]
async fn save_route_rejects_unsupported_and_unknown_explicit_effort() {
    let store = mem_store().await;
    let mut unsupported = route("unsupported");
    unsupported.targets[0] = ModelRouteTarget {
        provider: "anthropic".into(),
        model: "claude-opus-4-5".into(),
        effort: Some("max".into()),
    };
    let error = save_model_route(&store, unsupported).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "effort \"max\" is not supported for route target anthropic/claude-opus-4-5"
    );

    let mut unknown = route("unknown");
    unknown.targets[0] = ModelRouteTarget {
        provider: "openai".into(),
        model: "not-a-known-model".into(),
        effort: Some("high".into()),
    };
    let error = save_model_route(&store, unknown).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "effort \"high\" is not supported for route target openai/not-a-known-model"
    );
}

#[tokio::test]
async fn save_route_allows_unknown_model_when_effort_uses_model_default() {
    let store = mem_store().await;
    let mut unknown = route("unknown-default");
    unknown.targets[0] = ModelRouteTarget {
        provider: "openai".into(),
        model: "not-a-known-model".into(),
        effort: None,
    };
    assert!(save_model_route(&store, unknown).await.is_ok());
}

#[tokio::test]
async fn route_target_capabilities_are_unique_and_resolver_backed() {
    let store = mem_store().await;
    let resolved = list_model_route_target_capabilities(&store).await.unwrap();
    let opus = resolved
        .iter()
        .find(|item| item.provider == "anthropic" && item.model == "claude-opus-4-7")
        .unwrap();
    assert_eq!(
        opus.supported.iter().map(|option| option.value.as_str()).collect::<Vec<_>>(),
        vec!["low", "medium", "high", "max", "xhigh"]
    );
    assert_eq!(opus.provider_default.as_deref(), Some("high"));
    assert_eq!(
        resolved
            .iter()
            .filter(|item| item.provider == "anthropic" && item.model == "claude-opus-4-7")
            .count(),
        1
    );
}
```

- [ ] **Step 2: Run the focused tests and verify RED**

Run:

```sh
cargo test -p ryuzi-core llm_router::routes::tests::save_route_persists_explicit_effort_and_model_default -- --exact --nocapture
cargo test -p ryuzi-core llm_router::routes::tests::save_route_rejects_unsupported_and_unknown_explicit_effort -- --exact --nocapture
cargo test -p ryuzi-core llm_router::routes::tests::route_target_capabilities_are_unique_and_resolver_backed -- --exact --nocapture
```

Expected: the first test fails because `save_model_route` overwrites incoming effort from prior compatibility storage, the second fails because unsupported effort is accepted, and the third fails to compile because `list_model_route_target_capabilities` does not exist.

- [ ] **Step 3: Promote the contract and implement minimal backend validation**

Update `ModelRouteTarget` and add the DTO next to it:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouteTarget {
    /// Canonical provider family id, not a connection id.
    pub provider: String,
    pub model: String,
    /// Explicit route policy. `None` delegates to the model preference/default.
    #[serde(default)]
    pub effort: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouteTargetCapability {
    pub provider: String,
    pub model: String,
    pub supported: Vec<crate::llm_router::model_effort::ReasoningEffortOption>,
    pub provider_default: Option<String>,
}
```

Delete the `prior`, `old_targets`, and `used` compatibility-copy block from `save_model_route`. After `let mut next = sanitize_route(route)?;`, validate every target before assigning timestamps:

```rust
for target in &next.targets {
    validate_target_effort(store, target).await?;
}
```

Add these functions before `delete_model_route`:

```rust
async fn validate_target_effort(store: &Store, target: &ModelRouteTarget) -> anyhow::Result<()> {
    let Some(effort) = target.effort.as_deref() else {
        return Ok(());
    };
    let key = crate::llm_router::model_effort::ModelPreferenceKey {
        family: target.provider.clone(),
        model: target.model.clone(),
    };
    let capabilities = crate::llm_router::model_capabilities::resolve_for_model(store, &key).await?;
    if !capabilities.supports(effort) {
        anyhow::bail!(
            "effort {effort:?} is not supported for route target {}/{}",
            target.provider,
            target.model
        );
    }
    Ok(())
}

pub async fn list_model_route_target_capabilities(
    store: &Store,
) -> anyhow::Result<Vec<ModelRouteTargetCapability>> {
    let mut keys = std::collections::BTreeSet::new();
    for descriptor in crate::llm_router::registry::CATALOG {
        let family = descriptor.family.to_string();
        for model in descriptor.models {
            keys.insert((family.clone(), (*model).to_string()));
        }
    }
    for connection in crate::llm_router::connections::list_connections(store).await? {
        let Some(descriptor) = crate::llm_router::registry::descriptor(&connection.provider) else {
            continue;
        };
        for model in crate::llm_router::connections::effective_models(descriptor, &connection) {
            keys.insert((descriptor.family.to_string(), model));
        }
    }

    let mut result = Vec::with_capacity(keys.len());
    for (provider, model) in keys {
        let key = crate::llm_router::model_effort::ModelPreferenceKey {
            family: provider.clone(),
            model: model.clone(),
        };
        let capabilities =
            crate::llm_router::model_capabilities::resolve_for_model(store, &key).await?;
        result.push(ModelRouteTargetCapability {
            provider,
            model,
            supported: capabilities.supported,
            provider_default: capabilities.provider_default,
        });
    }
    Ok(result)
}
```

In `sanitize_route`, trim effort and reject explicit empty strings before filtering targets:

```rust
if target.effort.as_deref().is_some_and(|effort| effort.trim().is_empty()) {
    anyhow::bail!("route target effort cannot be empty; use Model default");
}
target.effort = target.effort.map(|effort| effort.trim().to_string());
```

- [ ] **Step 4: Run all route tests and verify GREEN**

Run:

```sh
cargo test -p ryuzi-core llm_router::routes::tests -- --nocapture
```

Expected: PASS. Existing route naming, family-head, ordering, and round-robin tests remain green; the new tests prove editable `Some`, resettable `None`, backend rejection, and resolver-backed DTO output.

- [ ] **Step 5: Commit the first-class route contract**

```sh
git add crates/core/src/llm_router/routes.rs
git commit -m "feat(core): validate route target effort"
```

Expected: one commit containing only route DTO, validation, capability listing, and focused tests.

---

### Task 2: Target-First Runtime Precedence and Route Execution

**Files:**
- Modify: `crates/core/src/llm_router/model_effort.rs:63-109,236-307,375-470,533-end`
- Modify: `crates/core/src/llm_router/client.rs:336-380,490-534,693-927, route construction call sites and inline tests`
- Modify: `crates/core/src/api/agent_api.rs:78-156,159-249`

**Interfaces:**
- Consumes: Task 1 persisted `ModelRouteTarget.effort` and existing `ModelPreferenceKey`, `ExecutionSurfaceKey`, and `EffectiveEffort`.
- Produces: `EffectiveEffortSource::RouteTarget` serialized as `routeTarget`.
- Produces: `TurnEffortPolicy { requested_model, caller_override, route_targets, configured, surfaces }`.
- Produces: `pub fn resolve_for_target(policy: &TurnEffortPolicy, route_target_key: Option<&RouteTargetEffortKey>, preference_key: &ModelPreferenceKey, surface: &ExecutionSurfaceKey) -> EffectiveEffort`.
- Invariant: `caller_override` is the already-resolved caller-level effort for a direct concrete-model request (request override over agent/session/project configuration, using the existing upstream order); Plan 5 does not add another caller-level setting. Once `route_target_key` is `Some`, every caller-level source is suppressed.

- [ ] **Step 1: Write failing pure precedence tests**

Add these tests to `model_effort.rs`, using the existing `capabilities()` helper and a policy fixture with one surface:

```rust
#[test]
fn route_target_beats_caller_and_global_preference() {
    let (surface, preference, target_key, mut policy) = precedence_policy();
    policy.caller_override = Some("low".into());
    policy.route_targets.insert(target_key.clone(), "high".into());
    policy.configured.insert(preference.clone(), "medium".into());

    let result = resolve_for_target(&policy, Some(&target_key), &preference, &surface);

    assert_eq!(result.value.as_deref(), Some("high"));
    assert_eq!(result.source, EffectiveEffortSource::RouteTarget);
}

#[test]
fn route_model_default_uses_global_then_provider_default() {
    let (surface, preference, target_key, mut policy) = precedence_policy();
    policy.caller_override = Some("low".into());
    policy.configured.insert(preference.clone(), "medium".into());

    let configured = resolve_for_target(&policy, Some(&target_key), &preference, &surface);
    assert_eq!(configured.value.as_deref(), Some("medium"));
    assert_eq!(configured.source, EffectiveEffortSource::Configured);

    policy.configured.clear();
    let provider = resolve_for_target(&policy, Some(&target_key), &preference, &surface);
    assert_eq!(provider.value.as_deref(), Some("high"));
    assert_eq!(provider.source, EffectiveEffortSource::Provider);
}

#[test]
fn concrete_model_still_honors_caller_override() {
    let (surface, preference, _, mut policy) = precedence_policy();
    policy.caller_override = Some("low".into());
    policy.configured.insert(preference.clone(), "medium".into());

    let result = resolve_for_target(&policy, None, &preference, &surface);

    assert_eq!(result.value.as_deref(), Some("low"));
    assert_eq!(result.source, EffectiveEffortSource::Project);
}
```

Define `precedence_policy()` in the test module with supported `low`, `medium`, and `high`, provider default `high`, route id `r1`, and target index `0`; do not use a mock store for these pure ordering tests.

- [ ] **Step 2: Write failing routed-execution regression tests**

In `client.rs`, update the existing named-route effort test and add one test that builds a route target with `effort: Some("high")`, a project/session caller override of `low`, and a global preference of `medium`. Assert both the resolved policy and request body use `high`. Add a second target with `effort: None` and assert it sends `medium`; clear the preference and assert it sends the resolver's provider default.

```rust
assert_eq!(explicit.effective_effort.as_deref(), Some("high"));
assert_eq!(explicit.effective_effort_source, EffectiveEffortSource::RouteTarget);
assert_eq!(explicit.request_body["reasoning"]["effort"], "high");
assert_eq!(model_default_with_preference.request_body["reasoning"]["effort"], "medium");
assert_eq!(model_default_without_preference.request_body["reasoning"]["effort"], "high");
```

Use the existing in-module test server/request capture fixture; do not assert a private helper without driving the real request path.

- [ ] **Step 3: Run focused tests and verify RED**

Run:

```sh
cargo test -p ryuzi-core llm_router::model_effort::tests::route_target_beats_caller_and_global_preference -- --exact --nocapture
cargo test -p ryuzi-core llm_router::model_effort::tests::route_model_default_uses_global_then_provider_default -- --exact --nocapture
cargo test -p ryuzi-core llm_router::client::tests::named_route_target_effort_controls_the_wire_request -- --exact --nocapture
```

Expected: pure tests fail because current order is caller -> compatibility target -> configured -> provider, and the request-path test fails because expanded route targets currently set `request_compatibility_effort: None` instead of carrying target policy.

- [ ] **Step 4: Implement the minimal target-first policy**

Rename `EffectiveEffortSource::RouteCompatibility` to `RouteTarget`, `project_override` to `caller_override`, and `route_compatibility` to `route_targets`. Update `selection_capabilities` to insert every stored route target effort into `route_targets` under `(route_id, original target index)`.

Replace `resolve_for_target` with this ordering:

```rust
pub fn resolve_for_target(
    policy: &TurnEffortPolicy,
    route_target_key: Option<&RouteTargetEffortKey>,
    preference_key: &ModelPreferenceKey,
    surface: &ExecutionSurfaceKey,
) -> EffectiveEffort {
    let Some(capabilities) = policy.surfaces.get(surface) else {
        return EffectiveEffort {
            value: None,
            label: None,
            source: EffectiveEffortSource::None,
            stored_status: Some(StoredEffortStatus::UnknownMetadata),
        };
    };
    let supported = |value: &str| capabilities.supported.iter().any(|option| option.value == value);
    let target_value = route_target_key
        .and_then(|key| policy.route_targets.get(key))
        .map(String::as_str);
    let caller = route_target_key
        .is_none()
        .then_some(policy.caller_override.as_deref())
        .flatten();
    let candidates = if route_target_key.is_some() {
        [
            (target_value, EffectiveEffortSource::RouteTarget),
            (
                policy.configured.get(preference_key).map(String::as_str),
                EffectiveEffortSource::Configured,
            ),
            (None, EffectiveEffortSource::None),
        ]
    } else {
        [
            (caller, EffectiveEffortSource::Project),
            (
                policy.configured.get(preference_key).map(String::as_str),
                EffectiveEffortSource::Configured,
            ),
            (None, EffectiveEffortSource::None),
        ]
    };
    let selected = candidates
        .into_iter()
        .find_map(|(value, source)| value.filter(|value| supported(value)).map(|value| (value.to_string(), source)))
        .or_else(|| resolved_surface_default(capabilities).map(|value| (value, EffectiveEffortSource::Provider)));
    let (value, source) = selected.map_or((None, EffectiveEffortSource::None), |(value, source)| (Some(value), source));
    let label = value.as_ref().and_then(|value| {
        capabilities.supported.iter().find(|option| &option.value == value).map(|option| option.label.clone())
    });
    EffectiveEffort {
        value,
        label,
        source,
        stored_status: Some(StoredEffortStatus::Valid),
    }
}
```

Remove `request_compatibility_effort` from `RouteTarget`, `resolved_requested_model`, and `resolve_target_effort`. In `expanded_route_targets` and every named-route expansion path, copy the route id/original index into `route_target_key`; policy lookup obtains the effort from `TurnEffortPolicy.route_targets` rather than duplicating it on the runtime target.

Update `selectable_native_models` route summaries to calculate each target's effective value in this order:

```rust
let selected = target.effort.clone()
    .filter(|value| capability.supported.iter().any(|option| option.value == *value))
    .map(|value| (Some(value), EffectiveEffortSource::RouteTarget))
    .or_else(|| configured.get(&preference_key).cloned().map(|value| (Some(value), EffectiveEffortSource::Configured)))
    .or_else(|| capability.provider_default.clone().map(|value| (Some(value), EffectiveEffortSource::Provider)))
    .unwrap_or((None, EffectiveEffortSource::None));
```

In `api/agent_api.rs`, named-route runtime info must ignore project/session stored effort and use `named_route_target_default` before global/provider summary. Concrete selections retain project/session behavior. Rename `named_route_compatibility_default` to `named_route_target_default` and return `None` when route targets do not resolve to one uniform effective value.

- [ ] **Step 5: Run policy, API, and request-path tests and verify GREEN**

Run:

```sh
cargo test -p ryuzi-core llm_router::model_effort::tests -- --nocapture
cargo test -p ryuzi-core llm_router::client::tests -- --nocapture
cargo test -p ryuzi-core api::agent_api::tests -- --nocapture
```

Expected: PASS. Named routes ignore caller effort, explicit targets win, Model default uses global then provider, concrete models retain caller override, and wire requests contain the resolved effort.

- [ ] **Step 6: Commit runtime precedence**

```sh
git add crates/core/src/llm_router/model_effort.rs crates/core/src/llm_router/client.rs crates/core/src/api/agent_api.rs
git commit -m "feat(core): enforce route target effort precedence"
```

Expected: one commit containing the policy rename, ordering change, execution propagation, API summaries, and regression tests.

---

### Task 3: Deterministic OpenAI Legacy Route Suffix Migration

**Files:**
- Modify: `crates/core/src/llm_router/routes.rs:58-65,339-388 and inline tests`

**Interfaces:**
- Consumes: Task 1 `validate_target_effort`, Plan 1 `resolve_for_model`, registry descriptors, and `connections::effective_models`.
- Produces: `pub(crate) fn parse_legacy_openai_route_suffix(model: &str) -> Option<(String, String)>`.
- Produces: `pub async fn migrate_legacy_route_target_effort(store: &Store) -> anyhow::Result<()>`.
- Uses completion marker setting key `llm_model_route_effort_migration_v1`; it does not add a numbered SQLite schema migration.
- Invariant: migration is convergent and never changes non-OpenAI, explicit-effort, unknown-base, or unsupported-effort targets.

- [ ] **Step 1: Write failing parser matrix tests**

Add pure parser tests to `routes.rs`:

```rust
#[test]
fn legacy_openai_route_suffix_parser_is_one_pass_and_longest_first() {
    let cases = [
        ("gpt-5-review-minimal", Some(("gpt-5-review", "minimal"))),
        ("gpt-5-xhigh-review", Some(("gpt-5", "xhigh"))),
        ("gpt-5-ultra", Some(("gpt-5", "ultra"))),
        ("gpt-5-medium", Some(("gpt-5", "medium"))),
        ("gpt-5-high", Some(("gpt-5", "high"))),
        ("gpt-5-low", Some(("gpt-5", "low"))),
        ("gpt-5-none", None),
        ("gpt-5-review", None),
        ("gpt-5-high-low", Some(("gpt-5-high", "low"))),
    ];
    for (model, expected) in cases {
        assert_eq!(
            parse_legacy_openai_route_suffix(model),
            expected.map(|(base, effort)| (base.to_string(), effort.to_string())),
            "{model}"
        );
    }
}
```

This locks the required operation order: remove one terminal `-review`, then remove exactly one effort suffix, with `minimal`, `medium`, `xhigh`, and `ultra` checked before shorter endings.

- [ ] **Step 2: Write failing idempotent migration tests**

Seed `llm_model_routes` directly so invalid legacy values bypass Task 1 save validation. Seed an OpenAI connection with model overrides and discovered effort metadata for `gpt-base`, `gpt-unsupported`, and an exact real `gpt-base-high` model. Assert this complete matrix after migration:

```rust
assert_eq!(target("migrate").model, "gpt-base");
assert_eq!(target("migrate").effort.as_deref(), Some("high"));
assert_eq!(target("non-openai").model, "gpt-base-high");
assert_eq!(target("explicit-wins").model, "gpt-base-high");
assert_eq!(target("explicit-wins").effort.as_deref(), Some("low"));
assert_eq!(target("unknown-base").model, "missing-high");
assert_eq!(target("unsupported").model, "gpt-unsupported-ultra");
assert_eq!(target("exact-real-model").model, "gpt-base-high");
```

Call `migrate_legacy_route_target_effort(&store)` twice and assert the serialized `llm_model_routes` value is identical after the second call. Assert `get_setting("llm_model_route_effort_migration_v1") == Some("done")`.

Add a malformed-route retry test: write `llm_model_routes = "{malformed"`, call migration, assert the error contains `expected` and `get_setting("llm_model_route_effort_migration_v1") == None`; replace the route setting with the valid matrix JSON, call migration again, and assert the normalized routes plus marker `Some("done")`. This proves a failed migration cannot suppress the next retry and requires no production test hook.

- [ ] **Step 3: Run migration tests and verify RED**

Run:

```sh
cargo test -p ryuzi-core llm_router::routes::tests::legacy_openai_route_suffix_parser_is_one_pass_and_longest_first -- --exact --nocapture
cargo test -p ryuzi-core llm_router::routes::tests::legacy_route_effort_migration_is_guarded_and_idempotent -- --exact --nocapture
cargo test -p ryuzi-core llm_router::routes::tests::legacy_route_effort_migration_marks_done_only_after_routes_persist -- --exact --nocapture
```

Expected: FAIL to compile because parser and migration functions do not exist.

- [ ] **Step 4: Implement the pure parser and guarded migration**

Add these constants and parser:

```rust
const ROUTE_EFFORT_MIGRATION_KEY: &str = "llm_model_route_effort_migration_v1";
const LEGACY_OPENAI_EFFORT_SUFFIXES: [&str; 6] = [
    "minimal", "medium", "xhigh", "ultra", "high", "low",
];

pub(crate) fn parse_legacy_openai_route_suffix(model: &str) -> Option<(String, String)> {
    let without_review = model.strip_suffix("-review").unwrap_or(model);
    LEGACY_OPENAI_EFFORT_SUFFIXES.iter().find_map(|effort| {
        without_review
            .strip_suffix(&format!("-{effort}"))
            .filter(|base| !base.is_empty())
            .map(|base| (base.to_string(), (*effort).to_string()))
    })
}
```

Add `known_models_for_family(store, family) -> anyhow::Result<BTreeSet<String>>`, collecting every registry descriptor model and every connection `effective_models` entry whose descriptor family matches. Exact full model IDs win: if the inventory contains the original suffix-bearing model, migration must leave it unchanged even when the parser can split it.

Implement migration without calling public `list_model_routes`, avoiding recursion:

```rust
pub async fn migrate_legacy_route_target_effort(store: &Store) -> anyhow::Result<()> {
    if store.get_setting(ROUTE_EFFORT_MIGRATION_KEY).await?.as_deref() == Some("done") {
        return Ok(());
    }
    let Some(raw) = store.get_setting(SETTING_KEY).await? else {
        store
            .set_setting(crate::domain::WriteOrigin::Agent, ROUTE_EFFORT_MIGRATION_KEY, "done")
            .await?;
        return Ok(());
    };
    let mut routes: Vec<ModelRouteInfo> = serde_json::from_str(&raw)?;
    let known = known_models_for_family(store, "openai").await?;
    let mut changed = false;
    for route in &mut routes {
        for target in &mut route.targets {
            if target.provider != "openai" || target.effort.is_some() || known.contains(&target.model) {
                continue;
            }
            let Some((base, effort)) = parse_legacy_openai_route_suffix(&target.model) else {
                continue;
            };
            if !known.contains(&base) {
                continue;
            }
            let key = crate::llm_router::model_effort::ModelPreferenceKey {
                family: "openai".into(),
                model: base.clone(),
            };
            let capabilities = crate::llm_router::model_capabilities::resolve_for_model(store, &key).await?;
            if !capabilities.supports(&effort) {
                continue;
            }
            target.model = base;
            target.effort = Some(effort);
            changed = true;
        }
    }
    if changed {
        persist_routes(store, &routes).await?;
    }
    store
        .set_setting(crate::domain::WriteOrigin::Agent, ROUTE_EFFORT_MIGRATION_KEY, "done")
        .await
}
```

Call `migrate_legacy_route_target_effort(store).await?` at the beginning of public `list_model_routes` and `save_model_route`; split current raw loading into private `load_model_routes_unmigrated` so migration never calls itself. This lazy post-schema migration runs after provider connections and Plan 1 metadata are available and remains safe if Plan 2-4 append schema versions before this plan executes.

- [ ] **Step 5: Run migration and route tests and verify GREEN**

Run:

```sh
cargo test -p ryuzi-core llm_router::routes::tests::legacy_openai_route_suffix_parser_is_one_pass_and_longest_first -- --exact --nocapture
cargo test -p ryuzi-core llm_router::routes::tests::legacy_route_effort_migration_is_guarded_and_idempotent -- --exact --nocapture
cargo test -p ryuzi-core llm_router::routes::tests -- --nocapture
```

Expected: PASS. The exact model, non-OpenAI, explicit effort, missing base, and unsupported effort cases remain unchanged; malformed JSON leaves the marker absent and retries after repair; valid OpenAI targets normalize once.

- [ ] **Step 6: Commit deterministic migration**

```sh
git add crates/core/src/llm_router/routes.rs
git commit -m "feat(core): migrate legacy route effort suffixes"
```

Expected: one commit containing parser, guarded migration, marker ordering, and full matrix tests; no schema migration or lockfile changes.

---

### Task 4: RPC, Tauri Command, and Generated Capability Contract

**Files:**
- Modify: `crates/core/src/api/connections_api.rs:40-55,135-150,230-252 and inline tests`
- Modify: `apps/cockpit/src-tauri/src/connections_cmd.rs:296-330`
- Modify: `apps/cockpit/src-tauri/src/lib.rs:120-140`
- Regenerate: `apps/cockpit/src/bindings.ts`

**Interfaces:**
- Consumes: Task 1 `routes::list_model_route_target_capabilities` and `ModelRouteTargetCapability`.
- Produces RPC: `list_model_route_target_capabilities` with `{}` params and `Vec<ModelRouteTargetCapability>` result.
- Produces Tauri command: `pub async fn list_model_route_target_capabilities(engine: Engine<'_>, runner_id: Option<String>) -> R<Vec<ModelRouteTargetCapability>>`.
- Produces TypeScript: `export type ModelRouteTargetCapability = { provider: string; model: string; supported: ReasoningEffortOption[]; providerDefault: string | null }`.

- [ ] **Step 1: Write the failing RPC dispatch test**

Add an API test that dispatches `list_model_route_target_capabilities` against the existing test state and deserializes the response:

```rust
let value = dispatch(&state, "list_model_route_target_capabilities", serde_json::json!({}))
    .await
    .unwrap();
let items: Vec<crate::llm_router::routes::ModelRouteTargetCapability> =
    serde_json::from_value(value).unwrap();
assert!(items.iter().any(|item| item.provider == "anthropic"
    && item.model == "claude-opus-4-7"
    && item.supported.iter().any(|option| option.value == "xhigh")));
```

- [ ] **Step 2: Run the API test and verify RED**

Run:

```sh
cargo test -p ryuzi-core api::connections_api::tests::lists_route_target_capabilities -- --exact --nocapture
```

Expected: FAIL with `unknown method: list_model_route_target_capabilities`.

- [ ] **Step 3: Add the RPC and thin Tauri proxy**

Add the method name to `CONNECTION_METHODS` and dispatch it with:

```rust
"list_model_route_target_capabilities" => {
    ok(routes::list_model_route_target_capabilities(cp.store()).await?)
}
```

Import `ModelRouteTargetCapability` in `connections_cmd.rs` and add:

```rust
#[tauri::command]
#[specta::specta]
pub async fn list_model_route_target_capabilities(
    engine: Engine<'_>,
    runner_id: Option<String>,
) -> R<Vec<ModelRouteTargetCapability>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc("list_model_route_target_capabilities", serde_json::json!({}))
        .await
}
```

Register `connections_cmd::list_model_route_target_capabilities` adjacent to `list_model_routes` in `apps/cockpit/src-tauri/src/lib.rs`.

- [ ] **Step 4: Run Rust tests and regenerate bindings**

Run:

```sh
cargo test -p ryuzi-core api::connections_api::tests::lists_route_target_capabilities -- --exact --nocapture
cargo test -p ryuzi-cockpit
cargo gen-bindings
rg -n "listModelRouteTargetCapabilities|ModelRouteTargetCapability|routeTarget" apps/cockpit/src/bindings.ts
```

Expected: tests PASS; generation reports `wrote .../apps/cockpit/src/bindings.ts`; `rg` finds the new command/type and the renamed `EffectiveEffortSource` literal `routeTarget`.

- [ ] **Step 5: Commit the cross-stack contract**

```sh
git add crates/core/src/api/connections_api.rs apps/cockpit/src-tauri/src/connections_cmd.rs apps/cockpit/src-tauri/src/lib.rs apps/cockpit/src/bindings.ts
git commit -m "feat(cockpit): expose route effort capabilities"
```

Expected: one commit containing API test, thin proxy, command registration, and generated bindings only.

---

### Task 5: Cockpit Conditional Effort Picker and Route Card Summaries

**Files:**
- Modify: `apps/cockpit/src/store-model-routes.ts:1-38`
- Modify: `apps/cockpit/src/views/ModelsView.tsx:1-46,345-428,387-600,603-672`
- Modify: `apps/cockpit/src/views/ModelsView.test.tsx:1-240 and route tests`

**Interfaces:**
- Consumes: generated `ModelRouteTargetCapability`, `ReasoningEffortOption`, and `commands.listModelRouteTargetCapabilities`.
- Produces store state: `targetCapabilities: ModelRouteTargetCapability[]` hydrated with routes.
- Produces pure helpers: `capabilityForTarget(capabilities, target)` and `nextTargetEffort(currentEffort, replacementCapability) -> string | null`.
- Invariant: picker options are `[{ value: "", label: "Model default" }, ...supported]`; empty UI value serializes as `null`.

- [ ] **Step 1: Write failing UI tests for visibility and exact options**

Extend the bindings mock with `listModelRouteTargetCapabilities` returning:

```ts
const targetCapabilities: ModelRouteTargetCapability[] = [
  {
    provider: "openai",
    model: "gpt-4.1",
    supported: [
      { value: "low", label: "Low", description: null },
      { value: "high", label: "High", description: null },
    ],
    providerDefault: "high",
  },
  {
    provider: "openai",
    model: "o3",
    supported: [{ value: "high", label: "High", description: null }],
    providerDefault: "high",
  },
  { provider: "anthropic", model: "claude-sonnet-4-5", supported: [], providerDefault: null },
];
```

Add tests using the existing `Edit` button and `ModelPicker` role contract:

```ts
test("route target shows Model default and only resolver-supported effort values", async () => {
  useModelRoutes.setState({ routes, targetCapabilities, loaded: true });
  render(<ModelsView />);
  fireEvent.click(screen.getByRole("button", { name: "Route" }));
  fireEvent.click(screen.getByRole("button", { name: "Edit" }));

  const effort = screen.getByRole("combobox", { name: "Effort for target 1" });
  fireEvent.click(effort);
  expect(screen.getAllByRole("option").map((option) => option.textContent)).toEqual([
    "Model default",
    "Low",
    "High",
  ]);
});

test("route target without effort capability renders no effort picker", async () => {
  useModelRoutes.setState({
    routes: [{
      ...routes[0],
      targets: [{ provider: "anthropic", model: "claude-sonnet-4-5", effort: null }],
    }],
    targetCapabilities,
    loaded: true,
  });
  useConnections.setState({
    catalog,
    connections: [{ ...anthropicApiConnection, models: ["claude-sonnet-4-5"] }],
    loaded: true,
  });
  render(<ModelsView />);
  fireEvent.click(screen.getByRole("button", { name: "Route" }));
  fireEvent.click(screen.getByRole("button", { name: "Edit" }));

  expect(screen.getByRole("combobox", { name: "Target 1" })).toBeTruthy();
  expect(screen.queryByRole("combobox", { name: "Effort for target 1" })).toBeNull();
});
```

These tests drive the rendered controls rather than invoking helpers directly.

- [ ] **Step 2: Write failing preservation, clearing, payload, and summary tests**

Add one interaction helper next to the tests:

```ts
async function chooseComboboxOption(name: string, option: string) {
  const combobox = screen.getByRole("combobox", { name });
  fireEvent.click(combobox);
  fireEvent.click(await screen.findByRole("option", { name: option }));
}
```

Add four focused tests:

```ts
test("changing target model preserves a compatible explicit effort", async () => {
  saveModelRoute.mockClear();
  useModelRoutes.setState({ routes, targetCapabilities, loaded: true });
  render(<ModelsView />);
  fireEvent.click(screen.getByRole("button", { name: "Route" }));
  fireEvent.click(screen.getByRole("button", { name: "Edit" }));
  await chooseComboboxOption("Effort for target 1", "High");
  await chooseComboboxOption("Target 1", "o3");
  fireEvent.click(screen.getByRole("button", { name: "Save route" }));

  await waitFor(() => expect(saveModelRoute).toHaveBeenCalled());
  expect(saveModelRoute.mock.calls.at(-1)?.[1].targets[0]).toEqual({
    provider: "openai",
    model: "o3",
    effort: "high",
  });
});

test("changing target model clears an incompatible effort to Model default", async () => {
  saveModelRoute.mockClear();
  useModelRoutes.setState({ routes, targetCapabilities, loaded: true });
  render(<ModelsView />);
  fireEvent.click(screen.getByRole("button", { name: "Route" }));
  fireEvent.click(screen.getByRole("button", { name: "Edit" }));
  await chooseComboboxOption("Effort for target 1", "Low");
  await chooseComboboxOption("Target 1", "o3");
  fireEvent.click(screen.getByRole("button", { name: "Save route" }));

  await waitFor(() => expect(saveModelRoute).toHaveBeenCalled());
  expect(saveModelRoute.mock.calls.at(-1)?.[1].targets[0]).toEqual({
    provider: "openai",
    model: "o3",
    effort: null,
  });
});

test("selecting Model default saves null", async () => {
  saveModelRoute.mockClear();
  useModelRoutes.setState({
    routes: [{ ...routes[0], targets: [{ provider: "openai", model: "gpt-4.1", effort: "high" }] }],
    targetCapabilities,
    loaded: true,
  });
  render(<ModelsView />);
  fireEvent.click(screen.getByRole("button", { name: "Route" }));
  fireEvent.click(screen.getByRole("button", { name: "Edit" }));
  await chooseComboboxOption("Effort for target 1", "Model default");
  fireEvent.click(screen.getByRole("button", { name: "Save route" }));

  await waitFor(() => expect(saveModelRoute).toHaveBeenCalled());
  expect(saveModelRoute.mock.calls.at(-1)?.[1].targets[0].effort).toBeNull();
});

test("route cards summarize explicit target overrides only", async () => {
  useModelRoutes.setState({
    routes: [{ ...routes[0], targets: [
      { provider: "openai", model: "gpt-4.1", effort: "high" },
      { provider: "openai", model: "o3", effort: null },
    ] }],
    targetCapabilities,
    loaded: true,
  });
  render(<ModelsView />);
  fireEvent.click(screen.getByRole("button", { name: "Route" }));
  expect(screen.getByText("High override")).toBeTruthy();
  expect(screen.queryByText("Model default", { selector: "[data-route-summary]" })).toBeNull();
});
```

- [ ] **Step 3: Run the focused UI tests and verify RED**

Run:

```sh
bun test apps/cockpit/src/views/ModelsView.test.tsx --test-name-pattern "route target|route cards|Model default"
```

Expected: FAIL because the store has no capability state, route rows have no effort Combobox, model changes always clear effort, and pills do not render override summaries.

- [ ] **Step 4: Hydrate resolver-backed target capabilities in the route store**

Update `ModelRoutesState`:

```ts
type ModelRoutesState = {
  routes: ModelRouteInfo[];
  targetCapabilities: ModelRouteTargetCapability[];
  loaded: boolean;
  hydrate: () => Promise<void>;
  save: (route: ModelRouteInfo) => Promise<boolean>;
  remove: (id: string) => Promise<boolean>;
};
```

Hydrate both commands concurrently and retain successful data independently:

```ts
hydrate: async () => {
  const [routesResult, capabilitiesResult] = await Promise.all([
    commands.listModelRoutes("local"),
    commands.listModelRouteTargetCapabilities("local"),
  ]);
  set({
    routes: routesResult.status === "ok" ? routesResult.data : [],
    targetCapabilities: capabilitiesResult.status === "ok" ? capabilitiesResult.data : [],
    loaded: true,
  });
  if (routesResult.status === "error") toast.error(`Routes failed: ${routesResult.error.message}`);
  if (capabilitiesResult.status === "error") toast.error(`Route effort capabilities failed: ${capabilitiesResult.error.message}`);
},
```

After successful save, re-run `listModelRouteTargetCapabilities` so provider discovery changes made during save/refresh cannot leave stale controls.

- [ ] **Step 5: Implement conditional picker, preservation, and summaries**

Import generated capability/option types. Add:

```ts
function capabilityForTarget(
  capabilities: ModelRouteTargetCapability[],
  target: { provider: string; model: string },
): ModelRouteTargetCapability | undefined {
  return capabilities.find((item) => item.provider === target.provider && item.model === target.model);
}

function nextTargetEffort(
  currentEffort: string | null,
  replacement: ModelRouteTargetCapability | undefined,
): string | null {
  return currentEffort && replacement?.supported.some((option) => option.value === currentEffort)
    ? currentEffort
    : null;
}
```

Pass `targetCapabilities` from `RouteTab` to `RouteForm` and `RouteCard`. In `setTarget`, preserve only compatible effort:

```ts
const replacement = capabilityForTarget(targetCapabilities, option);
const effort = nextTargetEffort(target.effort, replacement);
return { provider: option.provider, model: option.model, effort };
```

For each target row, render this immediately after `ModelPicker` only when `supported.length > 0`:

```tsx
<Combobox
  aria-label={`Effort for target ${index + 1}`}
  options={[
    { value: "", label: "Model default" },
    ...capability.supported.map((option) => ({
      value: option.value,
      label: option.label,
      description: option.description ?? undefined,
    })),
  ]}
  value={target.effort ?? ""}
  onValueChange={(value) => {
    setDraft((current) => ({
      ...current,
      targets: current.targets.map((candidate, candidateIndex) =>
        candidateIndex === index ? { ...candidate, effort: value || null } : candidate,
      ),
    }));
  }}
  className="w-[170px] shrink-0"
/>
```

Extend `RouteTargetPill` to accept capability metadata. If `target.effort` is non-null, find its resolver label and render `<span data-route-summary>{label ?? target.effort} override</span>` after the model. If effort is null, render no effort text. This preserves compact card dimensions and makes invalid historical values visible as raw overrides.

- [ ] **Step 6: Run all ModelsView tests and verify GREEN**

Run:

```sh
bun test apps/cockpit/src/views/ModelsView.test.tsx
```

Expected: PASS. Existing provider, endpoint, route CRUD, nested model-id, and confirmation behavior remain green alongside the new effort tests.

- [ ] **Step 7: Commit Cockpit controls**

```sh
git add apps/cockpit/src/store-model-routes.ts apps/cockpit/src/views/ModelsView.tsx apps/cockpit/src/views/ModelsView.test.tsx
git commit -m "feat(cockpit): edit route target effort"
```

Expected: one commit containing capability hydration, conditional picker, model-change rules, summaries, and component tests.

---

### Task 6: Browser Journey and Full Cross-Stack Verification

**Files:**
- Modify: `apps/cockpit/e2e/mock-ipc.ts`
- Modify: `apps/cockpit/e2e/app.e2e.ts`
- Verify: all files from Tasks 1-5

**Interfaces:**
- Consumes: generated command and Cockpit route editor from Tasks 4-5.
- Produces: one Playwright journey proving capability-driven editing survives the mocked IPC persistence boundary and appears in the route card.

- [ ] **Step 1: Write the failing Playwright route-effort journey**

Add this test to `apps/cockpit/e2e/app.e2e.ts` using the existing navigation helpers:

```ts
test("route target effort is capability-driven and summarized after save", async ({ page }) => {
  await page.getByRole("button", { name: "Models" }).click();
  await page.getByRole("button", { name: "Route" }).click();
  await page.getByRole("button", { name: "Edit route smart" }).click();

  await page.getByRole("combobox", { name: "Effort for target 1" }).click();
  await expect(page.getByRole("option")).toHaveText(["Model default", "Low", "High"]);
  await page.getByRole("option", { name: "High" }).click();
  await page.getByRole("button", { name: "Save route" }).click();

  await expect(page.getByText("High override")).toBeVisible();
  const calls = await mockCalls(page);
  expect(calls.filter((call) => call.cmd === "save_model_route").at(-1)?.args)
    .toMatchObject({ route: { targets: [{ provider: "openai", model: "gpt-4.1", effort: "high" }] } });
});
```

- [ ] **Step 2: Run the browser test and verify RED**

Run:

```sh
bun run --cwd apps/cockpit build
bun run --cwd apps/cockpit e2e:ci --grep "route target effort is capability-driven"
```

Expected: build passes, then Playwright FAILS because mock IPC does not yet implement `list_model_route_target_capabilities` or persist `save_model_route` target effort.

- [ ] **Step 3: Implement mock IPC capability and route persistence**

In `mock-ipc.ts`, add a `list_model_route_target_capabilities` fixture with OpenAI `gpt-4.1` options Low/High and provider default High. In the command switch, return the fixture for list calls. For `save_model_route`, replace the matching route in durable mock state with the exact submitted object and return the full route list; do not normalize or invent effort in the mock.

```ts
case "list_model_route_target_capabilities":
  return ok(fixtures.list_model_route_target_capabilities);
case "save_model_route": {
  const route = (args as { route: ModelRouteInfo }).route;
  durable.modelRoutes = durable.modelRoutes.map((current) => current.id === route.id ? route : current);
  return ok(durable.modelRoutes);
}
```

Initialize `durable.modelRoutes` from the route fixture when browser storage is absent.

- [ ] **Step 4: Run the browser journey and frontend gates**

Run:

```sh
bun run --cwd apps/cockpit e2e:ci --grep "route target effort is capability-driven"
bun test apps/cockpit/src/views/ModelsView.test.tsx
bun run typecheck
bun run --cwd apps/cockpit build
```

Expected: all commands PASS with no TypeScript errors. The journey observes exact supported options, saves `high`, and renders `High override`.

- [ ] **Step 5: Run Rust formatting, tests, and strict lint**

Run:

```sh
cargo fmt
cargo fmt --check
cargo test -p ryuzi-core
cargo test -p ryuzi-cockpit
cargo clippy -p ryuzi-core -p ryuzi-runner --all-targets -- -D warnings
```

Expected: all commands exit 0; no failed tests, formatting diff, warning, dead compatibility field, or stale `RouteCompatibility` variant remains.

- [ ] **Step 6: Run deterministic scope and placeholder guards**

Run:

```sh
rg -n "Compatibility-only|route_compatibility|RouteCompatibility|request_compatibility_effort" crates/core apps/cockpit/src
rg -n "minimal|medium|xhigh|ultra|high|low" crates/core/src/llm_router/routes.rs
rg -n "TBD|TODO|implement later|fill in details|similar to Task" docs/superpowers/plans/2026-07-12-agentic-05-route-effort.md
git diff --check
git status --short
```

Expected: the first command returns no matches; the second shows the one six-value longest-first migration constant and its matrix test; the placeholder scan returns no matches; `git diff --check` is silent; status lists only the scoped implementation files plus any unrelated pre-existing worktree entries.

- [ ] **Step 7: Commit browser coverage**

```sh
git add apps/cockpit/e2e/mock-ipc.ts apps/cockpit/e2e/app.e2e.ts
git commit -m "test(cockpit): cover route target effort journey"
```

Expected: one commit containing only mock IPC support and the route-effort browser journey.

- [ ] **Step 8: Commit verification-only formatting if needed**

First run:

```sh
git status --short
```

Expected: clean if earlier commits included formatted files. If and only if `cargo fmt` changed scoped Rust files, commit that mechanical diff:

```sh
git add crates/core/src/llm_router/routes.rs crates/core/src/llm_router/model_effort.rs crates/core/src/llm_router/client.rs crates/core/src/api/agent_api.rs crates/core/src/api/connections_api.rs apps/cockpit/src-tauri/src/connections_cmd.rs apps/cockpit/src-tauri/src/lib.rs
git commit -m "style: format route effort implementation"
```

Expected: no commit when clean; otherwise a formatting-only final commit.
