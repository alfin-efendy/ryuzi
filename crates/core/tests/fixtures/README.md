# Component Model test fixtures

These crates are intentionally independent Cargo workspaces so they do not become
members of the Ryuzi workspace. They compile small `wasm32-wasip2` components
against the canonical `ryuzi:plugin@0.1.0` lifecycle/types interfaces.

Build both fixtures from the repository root:

```sh
sh crates/core/tests/fixtures/build-components.sh
```

The script requires the Rust target below and prints each generated component path:

```sh
rustup target add wasm32-wasip2
```

Artifacts are written only below each fixture's `target/` directory:

- `component-noop/target/wasm32-wasip2/release/ryuzi_component_noop_fixture.wasm`
- `component-http-import/target/wasm32-wasip2/release/ryuzi_component_http_fixture.wasm`
- `component-connector/target/wasm32-wasip2/release/ryuzi_component_connector_fixture.wasm`
- `component-hooks/target/wasm32-wasip2/release/ryuzi_component_hooks_fixture.wasm`
- `component-hooks-loop/target/wasm32-wasip2/release/ryuzi_component_hooks_loop_fixture.wasm`

The script materializes `wit/deps/` from `crates/plugin-sdk/wit/` at build time.
Neither generated dependency files, Cargo lockfiles, nor `.wasm` artifacts are
committed. `component-noop` exports lifecycle only; `component-http-import`
contains a reachable HTTP call so its `ryuzi:http/http@0.1.0` import survives
component linking for policy tests. `component-connector` exports
`ryuzi:connector/connector@0.1.0` with `echo`/`slow`/`explode` tools (Task 9
connector adapter); `component-hooks` exports `ryuzi:hooks/hooks@0.1.0` and
branches on the payload text (`deny` → reject, `boom` → loop, `spinreject` →
spin then reject) to exercise the gating deny, fail-open-on-timeout, and
epoch-isolation paths. `component-hooks-loop` exports the same hooks interface
but its `handle` always loops (ignoring the payload); paired with
`component-hooks`'s `spinreject` it proves one component's timeout cannot trip
another's epoch deadline (per-component engines).

Each fixture exports only the single interface it is testing — component
runtimes reach a subset-exporting component's export via the per-interface
`GuestIndices`/`load` accessors, not the full `ryuzi:plugin` world.
