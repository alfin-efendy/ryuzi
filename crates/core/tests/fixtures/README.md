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

The script materializes `wit/deps/` from `crates/plugin-sdk/wit/` at build time.
Neither generated dependency files, Cargo lockfiles, nor `.wasm` artifacts are
committed. `component-noop` exports lifecycle only; `component-http-import`
contains a reachable HTTP call so its `ryuzi:http/http@0.1.0` import survives
component linking for policy tests.
