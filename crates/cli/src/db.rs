use crate::dispatch::Deps;

/// Quarantine a database created by the retired legacy schema (clean break:
/// it is moved aside, never migrated), then open the store. Every
/// DB-touching command goes through here.
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
