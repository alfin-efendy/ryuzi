//! Provider connections: a provider + credential + priority row the router
//! can route requests through. Secrets live in the `data` JSON blob.
use crate::router::registry::ProviderDescriptor;
use crate::store::Store;
use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionData {
    pub api_key: Option<String>,
    pub base_url_override: Option<String>,
    pub models_override: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConnectionRow {
    pub id: String,
    pub provider: String,
    pub auth_type: String,
    pub label: String,
    pub priority: i64,
    pub enabled: bool,
    pub data: ConnectionData,
    pub created_at: i64,
    pub updated_at: i64,
}

const COLS: &str = "id,provider,auth_type,label,priority,enabled,data,created_at,updated_at";

fn row_to_conn(r: &Row) -> rusqlite::Result<ConnectionRow> {
    let raw: String = r.get(6)?;
    Ok(ConnectionRow {
        id: r.get(0)?,
        provider: r.get(1)?,
        auth_type: r.get(2)?,
        label: r.get(3)?,
        priority: r.get(4)?,
        enabled: r.get::<_, i64>(5)? != 0,
        data: serde_json::from_str(&raw).unwrap_or_default(),
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
    })
}

pub async fn list_connections(store: &Store) -> anyhow::Result<Vec<ConnectionRow>> {
    store
        .with_conn(|c| -> rusqlite::Result<Vec<ConnectionRow>> {
            let mut stmt = c.prepare(&format!(
                "SELECT {COLS} FROM provider_connections ORDER BY priority ASC, created_at ASC"
            ))?;
            let items = stmt.query_map([], row_to_conn)?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(items)
        })
        .await
}

pub async fn get_connection(store: &Store, id: &str) -> anyhow::Result<Option<ConnectionRow>> {
    let id = id.to_string();
    store
        .with_conn(move |c| {
            c.query_row(
                &format!("SELECT {COLS} FROM provider_connections WHERE id=?1"),
                params![id],
                row_to_conn,
            )
            .optional()
        })
        .await
}

pub async fn add_connection(store: &Store, row: ConnectionRow) -> anyhow::Result<()> {
    let data = serde_json::to_string(&row.data)?;
    store
        .with_conn(move |c| {
            c.execute(
                "INSERT INTO provider_connections(id,provider,auth_type,label,priority,enabled,data,created_at,updated_at) \
                 VALUES (?1,?2,?3,?4,\
                   COALESCE((SELECT MAX(priority)+1 FROM provider_connections), 0),\
                   ?5,?6,?7,?8)",
                params![row.id, row.provider, row.auth_type, row.label,
                        row.enabled as i64, data, row.created_at, row.updated_at],
            )
            .map(|_| ())
        })
        .await
}

pub async fn update_connection(store: &Store, row: ConnectionRow) -> anyhow::Result<()> {
    let data = serde_json::to_string(&row.data)?;
    store
        .with_conn(move |c| {
            c.execute(
                "UPDATE provider_connections SET label=?2, enabled=?3, data=?4, updated_at=?5 WHERE id=?1",
                params![row.id, row.label, row.enabled as i64, data, row.updated_at],
            )
            .map(|_| ())
        })
        .await
}

pub async fn remove_connection(store: &Store, id: &str) -> anyhow::Result<()> {
    let id = id.to_string();
    store
        .with_conn(move |c| {
            c.execute("DELETE FROM provider_connections WHERE id=?1", params![id]).map(|_| ())
        })
        .await
}

/// Swap priority with the neighbor above (`dir=-1`) or below (`dir=1`).
pub async fn move_connection(store: &Store, id: &str, dir: i32) -> anyhow::Result<()> {
    let id = id.to_string();
    store
        .with_conn(move |c| {
            let tx = c.transaction()?;
            // Normalize priorities to 0..n first so swaps are well-defined.
            let ids: Vec<String> = {
                let mut stmt = tx.prepare(
                    "SELECT id FROM provider_connections ORDER BY priority ASC, created_at ASC",
                )?;
                let v = stmt
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                v
            };
            for (i, cid) in ids.iter().enumerate() {
                tx.execute(
                    "UPDATE provider_connections SET priority=?2 WHERE id=?1",
                    params![cid, i as i64],
                )?;
            }
            if let Some(pos) = ids.iter().position(|c2| *c2 == id) {
                let other = pos as i64 + dir as i64;
                if other >= 0 && (other as usize) < ids.len() {
                    tx.execute(
                        "UPDATE provider_connections SET priority=?2 WHERE id=?1",
                        params![ids[pos], other],
                    )?;
                    tx.execute(
                        "UPDATE provider_connections SET priority=?2 WHERE id=?1",
                        params![ids[other as usize], pos as i64],
                    )?;
                }
            }
            tx.commit()?;
            Ok(())
        })
        .await
}

pub fn effective_base_url(desc: &ProviderDescriptor, row: &ConnectionRow) -> Option<String> {
    row.data
        .base_url_override
        .clone()
        .or_else(|| desc.base_url.map(|s| s.to_string()))
        .map(|s| s.trim_end_matches('/').to_string())
}

pub fn effective_models(desc: &ProviderDescriptor, row: &ConnectionRow) -> Vec<String> {
    match &row.data.models_override {
        Some(m) if !m.is_empty() => m.clone(),
        _ => desc.models.iter().map(|s| s.to_string()).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    async fn mem_store() -> Store {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        // Keep the file alive by leaking the handle for the test's duration.
        let (_, path) = tmp.keep().unwrap();
        Store::open(&path).await.unwrap()
    }

    fn row(id: &str, provider: &str, prio: i64) -> ConnectionRow {
        ConnectionRow {
            id: id.into(), provider: provider.into(), auth_type: "api_key".into(),
            label: format!("{id} label"), priority: prio, enabled: true,
            data: ConnectionData {
                api_key: Some("sk-test".into()),
                base_url_override: None,
                models_override: None,
            },
            created_at: 1, updated_at: 1,
        }
    }

    #[tokio::test]
    async fn crud_and_priority_ordering() {
        let store = mem_store().await;
        add_connection(&store, row("c1", "openai", 0)).await.unwrap();
        add_connection(&store, row("c2", "anthropic", 1)).await.unwrap();
        let list = list_connections(&store).await.unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "c1"); // ordered by priority ASC
        assert_eq!(list[0].data.api_key.as_deref(), Some("sk-test"));

        // move c2 up → it becomes priority 0
        move_connection(&store, "c2", -1).await.unwrap();
        let list = list_connections(&store).await.unwrap();
        assert_eq!(list[0].id, "c2");

        let mut c1 = get_connection(&store, "c1").await.unwrap().unwrap();
        c1.label = "renamed".into();
        c1.data.models_override = Some(vec!["m1".into()]);
        update_connection(&store, c1).await.unwrap();
        assert_eq!(get_connection(&store, "c1").await.unwrap().unwrap().label, "renamed");

        remove_connection(&store, "c1").await.unwrap();
        assert_eq!(list_connections(&store).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn effective_helpers_prefer_overrides() {
        let desc = crate::router::registry::descriptor("openai").unwrap();
        let mut r = row("c1", "openai", 0);
        assert_eq!(effective_base_url(desc, &r).as_deref(), Some("https://api.openai.com/v1"));
        assert!(effective_models(desc, &r).contains(&"gpt-5.2".to_string()));
        r.data.base_url_override = Some("http://localhost:9/v1".into());
        r.data.models_override = Some(vec!["custom-model".into()]);
        assert_eq!(effective_base_url(desc, &r).as_deref(), Some("http://localhost:9/v1"));
        assert_eq!(effective_models(desc, &r), vec!["custom-model".to_string()]);
    }
}
