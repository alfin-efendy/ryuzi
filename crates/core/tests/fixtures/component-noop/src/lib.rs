wit_bindgen::generate!({
    path: "wit",
    world: "noop",
    generate_all,
});

struct Fixture;

impl exports::ryuzi::plugin::lifecycle::Guest for Fixture {
    fn init() -> Result<
        exports::ryuzi::plugin::lifecycle::LifecycleState,
        exports::ryuzi::plugin::lifecycle::PluginError,
    > {
        Ok(exports::ryuzi::plugin::lifecycle::LifecycleState {
            initialized: true,
            version: "0.1.0".to_string(),
        })
    }

    fn health() -> Result<
        exports::ryuzi::plugin::lifecycle::HealthStatus,
        exports::ryuzi::plugin::lifecycle::PluginError,
    > {
        Ok(exports::ryuzi::plugin::lifecycle::HealthStatus {
            healthy: true,
            message: "ready".to_string(),
        })
    }

    fn migrate(
        _from_version: String,
    ) -> Result<
        exports::ryuzi::plugin::lifecycle::LifecycleState,
        exports::ryuzi::plugin::lifecycle::PluginError,
    > {
        Self::init()
    }

    fn shutdown() -> Result<
        exports::ryuzi::plugin::lifecycle::LifecycleState,
        exports::ryuzi::plugin::lifecycle::PluginError,
    > {
        Ok(exports::ryuzi::plugin::lifecycle::LifecycleState {
            initialized: false,
            version: "0.1.0".to_string(),
        })
    }
}

export!(Fixture);
