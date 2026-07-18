# WASM Component Plugin Platform Design

**Status:** Proposed for review  
**Date:** 2026-07-17

## Purpose

Replace Ryuzi's mixed declarative/native plugin model with independently released, signed WebAssembly Component Model bundles. The platform must support all plugin capability axes—gateway, connector/MCP tools, model provider, and lifecycle hooks—without special-case runtime wiring for Discord or static Rust catalogs for providers.

The first-party rollout validates the platform with Mimo and OpenCode bootstrap providers, GitHub, Discord, Atlassian (Jira and Confluence), Bitbucket Cloud, and then the remaining model providers. Every plugin version and deployment is independent of the Ryuzi application release.

## Goals

- Define a WIT Component Model plugin ABI starting at `0.1.0`.
- Install, activate, update, pin, roll back, revoke, and diagnose plugins independently from the desktop application.
- Enforce manifest-declared least-privilege host capabilities, including outbound-network allowlists.
- Keep secrets and OAuth access/refresh tokens in host-controlled secret storage; components never receive raw credentials.
- Support hybrid lifetimes: long-lived gateway/provider instances and stateless connector/hook calls.
- Remove Discord-specific daemon, registry, settings, and default-gateway wiring.
- Replace static Rust provider registration with WASM provider plugins.

## Non-goals

- Third-party publisher onboarding in the initial delivery. The initial trust root is first-party release keys.
- Full GitHub CLI parity in the first GitHub release. Parity is delivered by independently versioned plugin releases.
- Arbitrary local, unsigned developer plugins in the production install path.
- Preserving subprocess extensions as a permanent plugin execution model. Their migration or sunset is a separate follow-up once their equivalent WIT capability is available.

## Current-state constraints

`ryuzi-plugin-sdk` currently owns a declarative TOML manifest and validation. `ryuzi-core` binds that manifest to `CorePlugin` trait-object capabilities. Native Discord is constructed in `plugins::builtin`, injected into the daemon through Discord factory code and global `enabled_gateways` settings. Model providers are generated from a static Rust catalog. Existing installer, remote catalog, ledger, pinning, update, revocation, and doctor concepts should be evolved rather than replaced wholesale.

## Architecture

### Plugin bundle and release metadata

Each installable release is a signed bundle with a plugin-specific version and deployment channel:

```text
<plugin-id>-<version>.bundle
  ryuzi-plugin.toml
  plugin.wasm
  release.json
  plugin.sig
```

- `ryuzi-plugin.toml` declares identity, publisher, version, UI metadata, plugin settings, capability exports, OAuth profiles, network allowlists, and requested resource limits.
- `plugin.wasm` is a WASI Preview 2 component implementing the declared Ryuzi WIT worlds.
- `release.json` records the plugin version, channel, supported host/WIT API range, artifact URL, SHA-256, signing key ID, changelog, and revocation/compatibility metadata.
- `plugin.sig` signs the release metadata and artifact identity. The host verifies it against a pinned first-party trust root.

The signed remote catalog is an index of release metadata, not embedded plugin code. It provides available releases, their locations, compatibility metadata, and revocation notices.

### Installation, activation, and version management

Cockpit installation follows this pipeline:

1. Resolve a requested plugin release from the signed catalog.
2. Download it to an isolated staging directory.
3. Verify catalog trust, release signature/key ID, artifact hash, manifest validity, declared WIT range, actual component imports/exports, and requested permissions.
4. Present the plugin's capabilities, network domains, OAuth scopes/profiles, storage quota, and long-lived status for user approval.
5. Atomically store the verified release at `plugins/<id>/<version>/` and update the active-version pointer.
6. Record source, version, checksums, timestamps, pin status, and active version in the install ledger.
7. Request a daemon restart when a long-lived capability must be created, replaced, or removed.

Updates are opt-in during `0.x`. Pinning prevents movement of the active pointer. Rollback only selects a locally retained, valid, non-revoked release. A failed activation leaves the old active release unchanged. A revoked release cannot be newly activated, re-enabled, or selected for rollback.

### Bootstrap

On first application install, the same signed catalog and installer pipeline attempts to install the `mimo` and `opencode` provider bundles. The application install itself does not fail if bootstrap downloads fail. Onboarding exposes retry/recovery and may proceed once at least one bootstrap provider is installed and configured.

### Runtime host

`ryuzi-core` introduces a generic Component Model host and per-capability adapters. It validates a component before activation, creates only the host imports authorized by the manifest and policy, and maps component values to validated domain values.

The host has no plugin-ID-specific code. Capability discovery comes from the manifest plus WIT exports. `CorePlugin` is evolved or replaced by an installed-component representation that preserves PluginHost's identity, ordering, enablement, ledger, and doctor responsibilities while replacing native trait objects with generic adapters.

#### Hybrid lifecycle

- **Long-lived:** gateways and providers that declare this lifecycle receive one supervised instance for the daemon lifetime. The host calls `start`, drives events, exposes health, and calls `stop` for shutdown, disablement, or upgrade.
- **Stateless:** connector/MCP and hook invocations are isolated calls. Persistent state must use the scoped host storage capability.
- **Failure containment:** traps, panics, timeouts, resource breaches, and invalid outputs never crash the daemon. The host records redacted diagnostic data; long-lived instances are stopped and restarted with exponential backoff. `tool.before` hooks fail open on an unavailable component.

The host enforces defaults that a manifest may only tighten: fuel/CPU, wall-clock timeouts, memory, concurrent invocation count, storage, request rate, and outbound payload limits.

## WIT contract

All WIT packages begin pre-1.0:

```text
ryuzi:plugin@0.1.0
ryuzi:host@0.1.0
ryuzi:settings@0.1.0
ryuzi:storage@0.1.0
ryuzi:http@0.1.0
ryuzi:oauth@0.1.0
ryuzi:gateway@0.1.0
ryuzi:connector@0.1.0
ryuzi:provider@0.1.0
ryuzi:hooks@0.1.0
```

A plugin declares a supported ABI range such as `>=0.1.0, <0.2.0`. During `0.x`, incompatible ABI changes increment the minor version; compatible additions and fixes use patch versions. The host rejects incompatible components before activation. The ABI advances to `1.0.0` only after the pilot capability surfaces are stable; a later breaking change requires the next major version.

### Shared exports and imports

Every component exports `init`, `health`, `migrate`, and `shutdown`. Its world imports only the capabilities it has requested and been authorized to use.

| Capability | Plugin exports | Authorized host services |
| --- | --- | --- |
| Gateway | start, stop, deliver outbound, health | inbound/outbound gateway events, connection status, scoped settings/storage, authorized HTTP/OAuth |
| Connector | enumerate MCP servers/tools, invoke tool | settings/storage, authorized HTTP/OAuth, approvals, logging |
| Provider | enumerate models, complete, stream | credential-backed/authorized transport, quotas, logging |
| Hooks | handle typed lifecycle events | settings/storage, authorized HTTP/OAuth, logging |
| Shared | init, health, migrate, shutdown | time/IDs, logging, telemetry, approvals, scoped configuration |

Host validation applies to every inbound and outbound record. Components do not receive raw sockets, general filesystem access, process environment access, subprocess access, or unrestricted networking. WASI filesystem access, if needed, is a plugin-private storage directory only.

### Network and secrets

The manifest declares exact API domains or approved wildcard suffixes and an explanatory reason. Host HTTP checks the initial target and every redirect against this allowlist, applies rate and payload limits, and redacts secrets from logs and telemetry.

Plugin setting and storage namespaces are isolated by plugin ID. Sensitive values live only in host secret storage. Components receive neither raw access tokens nor refresh tokens. They issue an authorized HTTP request through an OAuth profile; the host injects credentials, refreshes them when necessary, filters sensitive headers, and returns only the HTTP result.

### OAuth

The host supports authorization-code with PKCE and OAuth Device Authorization. Plugins declare OAuth profiles, endpoints, scopes, and allowed flows. Cockpit can perform PKCE via browser and local callback. Device Flow exposes a verification URL and user code to the interactive client while the host performs expiry-aware polling; codes and tokens are never persisted in diagnostic logs.

Manual-token or environment-based credentials may be declared as a plugin-specific alternative. The host writes such values to secret storage and still exposes only authorized requests to the component.

## First-party pilots

| Plugin | Initial capability | Authentication | Notes |
| --- | --- | --- | --- |
| `mimo` | Provider | Provider-defined host credential profile | Bootstrap plugin |
| `opencode` | Provider | Provider-defined host credential profile | Bootstrap plugin |
| `github` | Connector/tools, hooks, HTTP API | Device Flow by default; PKCE and manual token fallback | Releases incrementally toward `gh` CLI parity |
| `discord` | Long-lived gateway | Bot token in host secret storage | Reference gateway migration; no native special case |
| `atlassian` | Connector/tools for Jira and Confluence | Shared Atlassian Cloud OAuth 2.0 3LO profile | One Atlassian login for Jira and Confluence |
| `bitbucket` | Connector/tools for Bitbucket Cloud | Bitbucket Cloud OAuth consumer/profile | Separate bundle and OAuth boundary |
| Remaining providers | Provider | Provider-specific host credential profile | Port after bootstrap proof |

### GitHub release roadmap

- **0.1.x foundation:** login/logout/status, profile, repository browse/clone metadata, issues, pull requests, REST/GraphQL API access, and host approvals.
- **0.2.x delivery:** releases, workflows/runs, gists, labels, secrets/variables, SSH/GPG keys, and caches.
- **0.3.x collaboration:** projects, organizations/teams, rulesets, notifications, search, and discussions.
- **0.4.x and later:** the remaining relevant `gh` domains, enterprise hostname support, and curated extension-equivalent tool bundles.

Mutating operations—including merge/delete, publishing releases, secret changes, workflow dispatch, and organization changes—must be marked risk-bearing and use Ryuzi's approval flow before an authorized request is sent.

### Atlassian and Bitbucket boundary

`atlassian` includes Jira and Confluence because they use the same Atlassian Cloud 3LO authorization model. `bitbucket` is a separate plugin because Bitbucket Cloud uses its own OAuth consumer, authorization endpoint, token endpoint, scopes, and API domain. A single user token must not be assumed to work across all three products.

## Discord migration

The `discord` bundle is a generic long-lived gateway component. It declares its bot token setting, Discord network endpoints, required resource limits, and gateway lifecycle in its manifest. It translates Discord protocol events into typed gateway WIT records; the generic host maps those records to Ryuzi domain events and sends outbound domain events back through the same WIT export.

Migration removes `gateway::discord`, `builtin::discord_plugin`, static Discord config fields, the default `enabled_gateways = discord`, and daemon composition code that selects Discord by ID. Gateway enablement becomes generic installed-plugin state. Connection health, reconnect/backoff, idempotency, graceful stop, and doctor output are all handled through generic gateway supervision.

A temporary native fallback may exist only while a capability migration is verified. No fallback, registry path, or daemon selector may branch on `plugin.id == "discord"`.

## Provider migration

Provider manifests and model metadata move from the static Rust provider catalog into individual provider bundles. A generic provider adapter maps WIT `models`, `complete`, and streaming calls into the LLM router. `mimo` and `opencode` prove this end to end; remaining providers migrate independently. Once each native provider path is replaced, its static registration is removed. The completed system does not depend on a hardcoded provider catalog for runtime discovery.

## Cockpit and runner behavior

Cockpit remains the management UI. Browse and detail screens show publisher verification, active version, available update, pin/rollback/uninstall actions, capabilities, allowlisted domains, OAuth connections/scopes, health, and redacted doctor/log data. Installation requests permission approval before activation.

`atlassian` presents a single **Connect Jira & Confluence** connection. `bitbucket` presents a separate Bitbucket connection. GitHub defaults to Device Flow and can use PKCE where enabled.

Installing, enabling, disabling, upgrading, or removing a long-lived plugin asks for a daemon restart. Stateless plugin changes apply to newly created sessions after loading completes. The runner gains no separate plugin-management UX; it can relay transient Device Flow information through daemon/client protocol without retaining user codes or tokens in durable logs.

## Failure behavior

- A missing catalog does not affect locally verified installed bundles; new install/update attempts fail with retryable diagnostics.
- Bootstrap provider download failure leaves the application installed and recoverable.
- Failed, cancelled, or expired OAuth marks only its profile unavailable and clearly identifies the required reconnection.
- Trap, timeout, quota, or validation failure is isolated to the invocation or supervised instance.
- Failed update preserves the prior active version.
- Revocation stops the affected gateway at the next safe boundary, blocks future enablement/rollback, and displays the signed revocation reason.

## Verification and acceptance criteria

1. The host rejects invalid manifests, signatures, hashes, WIT ranges, import/export contracts, and permission mismatches before activation.
2. Automated tests cover network allowlists and redirects, storage isolation, secret non-disclosure, OAuth refresh, CPU/memory/timeout limits, traps, and long-lived recovery/backoff.
3. Test components exercise each typed WIT adapter and demonstrate host input/output validation.
4. Mimo and OpenCode bootstrap through the normal catalog installer; a failed bootstrap can be retried without reinstalling the app.
5. GitHub `0.1.x` supports auth/profile/repository/issues/pull requests/API with Device Flow, PKCE, and approvals for mutations.
6. Atlassian Jira/Confluence share one 3LO connection; Bitbucket is a separately installable plugin with its own OAuth profile.
7. Discord operates via generic long-lived gateway lifecycle, with no Discord ID selector, default configuration, factory wiring, or builtin registration remaining.
8. Model providers load from WASM bundles through a generic adapter, with no runtime provider discovery from a static Rust catalog.
9. Per-plugin update, pin, rollback, revocation, and doctor behavior work across independently released bundle versions without an application update.
