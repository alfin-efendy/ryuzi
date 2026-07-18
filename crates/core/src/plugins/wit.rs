#[cfg(test)]
mod tests {
    mod bindings {
        wasmtime::component::bindgen!({
            path: "../plugin-sdk/wit",
            world: "plugin",
        });
    }

    use bindings::ryuzi::plugin::types::HealthStatus;

    #[test]
    fn generated_health_status_and_plugin_bindings_are_available() {
        let status = HealthStatus {
            healthy: true,
            message: "ready".to_owned(),
        };
        assert!(status.healthy);
        assert_eq!(status.message, "ready");

        let _: Option<bindings::Plugin> = None;
    }
}
