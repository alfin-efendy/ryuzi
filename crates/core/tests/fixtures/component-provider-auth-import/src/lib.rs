wit_bindgen::generate!({
    path: "wit",
    world: "provider-auth-import",
    generate_all,
});

struct Fixture;

impl exports::ryuzi::plugin::lifecycle::Guest for Fixture {
    fn init() -> Result<
        exports::ryuzi::plugin::lifecycle::LifecycleState,
        exports::ryuzi::plugin::lifecycle::PluginError,
    > {
        // Force the `ryuzi:provider-auth` import to be retained: wit-bindgen
        // only emits an import the guest actually references, so this call is
        // what makes the component declare the import the host must satisfy.
        // The request never leaves the host — instantiation is what this
        // fixture exists to prove — but the import edge is real.
        let _ = ryuzi::provider_auth::provider_auth::authorized_request(
            "unused",
            &ryuzi::provider_auth::provider_auth::ProviderRequest {
                method: "GET".to_string(),
                url: "https://unused.invalid/".to_string(),
                headers: Vec::new(),
                body: None,
            },
        );
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
