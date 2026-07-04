use std::collections::HashMap;
use std::sync::Arc;

/// A `name → factory` registry, generic over the (unsized) factory trait object.
/// One instance backs each extension axis (harness, gateway, connector).
pub struct Registry<F: ?Sized> {
    entries: HashMap<String, Arc<F>>,
}

impl<F: ?Sized> Registry<F> {
    pub fn new() -> Self {
        Registry {
            entries: HashMap::new(),
        }
    }

    /// Register (or replace) a factory under `name`.
    pub fn register(&mut self, name: impl Into<String>, factory: Arc<F>) {
        self.entries.insert(name.into(), factory);
    }

    /// Look up a factory by name.
    pub fn get(&self, name: &str) -> Option<Arc<F>> {
        self.entries.get(name).cloned()
    }

    /// All registered names, sorted (stable output for UI / `doctor`).
    pub fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.entries.keys().cloned().collect();
        v.sort();
        v
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl<F: ?Sized> Default for Registry<F> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A minimal unsized trait to exercise the generic registry directly.
    trait Greeter: Send + Sync {
        fn hello(&self) -> &str;
    }
    struct En;
    impl Greeter for En {
        fn hello(&self) -> &str {
            "hi"
        }
    }

    #[test]
    fn register_get_and_names_sorted() {
        let mut reg: Registry<dyn Greeter> = Registry::new();
        assert!(reg.is_empty());
        reg.register("zeta", Arc::new(En));
        reg.register("alpha", Arc::new(En));
        assert_eq!(reg.len(), 2);
        assert_eq!(reg.get("alpha").unwrap().hello(), "hi");
        assert!(reg.get("missing").is_none());
        assert_eq!(reg.names(), vec!["alpha".to_string(), "zeta".to_string()]);
    }

    #[test]
    fn register_replaces_same_name() {
        let mut reg: Registry<dyn Greeter> = Registry::new();
        reg.register("x", Arc::new(En));
        reg.register("x", Arc::new(En));
        assert_eq!(reg.len(), 1, "same name replaces, not duplicates");
    }
}
