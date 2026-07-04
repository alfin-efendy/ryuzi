fn main() {
    // Expose the compile target so the resolver picks the right standalone artifact.
    println!(
        "cargo:rustc-env=RYUZI_TARGET={}",
        std::env::var("TARGET").unwrap()
    );
    println!("cargo:rerun-if-changed=sidecar.manifest.json");
}
