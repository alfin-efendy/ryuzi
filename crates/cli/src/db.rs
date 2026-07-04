use crate::dispatch::Deps;

/// Quarantine a legacy TS-schema database (clean break, Spec 4 §6), then open
/// the store. Every DB-touching command goes through here.
pub(crate) async fn open_store(deps: &mut Deps) -> anyhow::Result<ryuzi_core::Store> {
    match ryuzi_core::store::quarantine_legacy_db(&deps.db_path) {
        Ok(Some(bak)) => (deps.err)(&format!(
            "note: existing database used the retired schema; moved to {}",
            bak.display()
        )),
        Ok(None) => {}
        Err(e) => anyhow::bail!(e),
    }
    ryuzi_core::Store::open(&deps.db_path).await
}
