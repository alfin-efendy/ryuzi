use ryuzi_core::sidecar::{
    default_bun_probe, HttpFetcher, SidecarConfig, SidecarManager, SidecarManifest,
};

pub fn embedded_manifest() -> SidecarManifest {
    serde_json::from_str(include_str!("../sidecar.manifest.json"))
        .expect("sidecar.manifest.json is validated at build time")
}

pub fn release_tag() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

pub fn manager() -> SidecarManager {
    SidecarManager::new(
        SidecarConfig {
            manifest: embedded_manifest(),
            cache_dir: ryuzi_core::paths::state_dir().join("sidecars"),
            target: env!("RYUZI_TARGET").to_string(),
            release_tag: release_tag(),
            override_path: std::env::var_os("RYUZI_ACP_PATH").map(std::path::PathBuf::from),
            bun_probe: default_bun_probe,
        },
        Box::new(HttpFetcher),
    )
}
