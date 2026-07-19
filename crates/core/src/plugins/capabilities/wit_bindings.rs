//! Generated Wasmtime component-model host bindings for the `ryuzi:plugin`
//! world's host-provided imports (`host`, `settings`, `storage`,
//! `http`/`oauth`). Only `host`, `settings`, and `storage` are linked by this
//! slice — see `runtime.rs`'s `instantiate` for which `add_to_linker_instance`
//! calls are actually wired in, gated by [`super::super::runtime::HostPolicy`].
//!
//! Kept in its own file (rather than inlined in `mod.rs`) because
//! `wasmtime::component::bindgen!` output is large and is not meant to be
//! read/edited by hand.
//!
//! Generated module paths used elsewhere in `capabilities` (relative to this
//! module): `ryuzi::host::host`, `ryuzi::settings::settings`,
//! `ryuzi::storage::storage`, each exposing a `Host` trait, its
//! request/response record and error types, and `add_to_linker_instance`.
//! `add_to_linker_instance::<T, D>` requires `D: HasData` with
//! `for<'a> D::Data<'a>: Host` — the state type `T` itself is *not* the
//! `HasData` marker; `wasmtime::component::HasSelf<T>` is, and is what every
//! call site here passes as `D` (confirmed by direct probe, not assumed).

wasmtime::component::bindgen!({
    path: "../plugin-sdk/wit",
    world: "plugin",
});
