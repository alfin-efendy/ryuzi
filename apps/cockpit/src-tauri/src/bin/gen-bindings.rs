//! Regenerates `apps/cockpit/src/bindings.ts` headlessly.
//!
//! Exists as a bin (not just the lib test) because on Windows the cargo
//! lib-test harness dies at startup with STATUS_ENTRYPOINT_NOT_FOUND
//! (tauri-apps/tauri#13419); bin artifacts get the app manifest linked.
fn main() {
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/bindings.ts");
    ryuzi_cockpit_lib::export_bindings(&out);
    println!("wrote {}", out.display());
}
