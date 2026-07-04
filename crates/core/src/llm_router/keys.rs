//! Endpoint API keys gating the local router. Plaintext by design: the
//! config-apply feature must re-read the literal key to write it into agent
//! configs. OS-keychain encryption is recorded as future work in the spec.
use crate::paths::{new_id, now_ms};
use crate::store::Store;
use rusqlite::{params, OptionalExtension, Row};

#[derive(Debug, Clone, PartialEq)]
pub struct EndpointKey {
    pub id: String,
    pub name: String,
    pub key: String,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}

pub fn generate_key() -> String {
    format!("ryz-{}", uuid::Uuid::new_v4().simple())
}

const COLS: &str = "id,name,key,created_at,last_used_at";

fn row_to_key(r: &Row) -> rusqlite::Result<EndpointKey> {
    Ok(EndpointKey {
        id: r.get(0)?,
        name: r.get(1)?,
        key: r.get(2)?,
        created_at: r.get(3)?,
        last_used_at: r.get(4)?,
    })
}

pub async fn list_keys(store: &Store) -> anyhow::Result<Vec<EndpointKey>> {
    store
        .with_conn(|c| -> rusqlite::Result<Vec<EndpointKey>> {
            let mut stmt =
                c.prepare(&format!("SELECT {COLS} FROM endpoint_keys ORDER BY created_at ASC"))?;
            let items = stmt.query_map([], row_to_key)?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(items)
        })
        .await
}

pub async fn create_key(store: &Store, name: &str) -> anyhow::Result<EndpointKey> {
    let k = EndpointKey {
        id: new_id(),
        name: name.to_string(),
        key: generate_key(),
        created_at: now_ms(),
        last_used_at: None,
    };
    let row = k.clone();
    store
        .with_conn(move |c| {
            c.execute(
                "INSERT INTO endpoint_keys(id,name,key,created_at,last_used_at) VALUES (?1,?2,?3,?4,NULL)",
                params![row.id, row.name, row.key, row.created_at],
            )
            .map(|_| ())
        })
        .await?;
    Ok(k)
}

pub async fn revoke_key(store: &Store, id: &str) -> anyhow::Result<()> {
    let id = id.to_string();
    store
        .with_conn(move |c| {
            c.execute("DELETE FROM endpoint_keys WHERE id=?1", params![id]).map(|_| ())
        })
        .await
}

/// True when `presented` matches a stored key; bumps `last_used_at`.
pub async fn verify_key(store: &Store, presented: &str) -> anyhow::Result<bool> {
    let presented = presented.to_string();
    let now = now_ms();
    store
        .with_conn(move |c| {
            let n = c.execute(
                "UPDATE endpoint_keys SET last_used_at=?2 WHERE key=?1",
                params![presented, now],
            )?;
            Ok(n > 0)
        })
        .await
}

/// Oldest key — the one config-apply writes into agent configs.
pub async fn first_key(store: &Store) -> anyhow::Result<Option<EndpointKey>> {
    store
        .with_conn(|c| {
            c.query_row(
                &format!("SELECT {COLS} FROM endpoint_keys ORDER BY created_at ASC LIMIT 1"),
                [],
                row_to_key,
            )
            .optional()
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    async fn mem_store() -> Store {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let (_, path) = tmp.keep().unwrap();
        Store::open(&path).await.unwrap()
    }

    #[test]
    fn generated_keys_have_prefix_and_entropy() {
        let a = generate_key();
        let b = generate_key();
        assert!(a.starts_with("ryz-") && a.len() == 4 + 32);
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn create_list_verify_revoke() {
        let store = mem_store().await;
        assert!(first_key(&store).await.unwrap().is_none());
        let k = create_key(&store, "dev").await.unwrap();
        assert_eq!(k.name, "dev");
        assert!(verify_key(&store, &k.key).await.unwrap());
        assert!(!verify_key(&store, "ryz-wrong").await.unwrap());
        // verify bumps last_used_at
        let listed = list_keys(&store).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].last_used_at.is_some());
        assert_eq!(first_key(&store).await.unwrap().unwrap().id, k.id);
        revoke_key(&store, &k.id).await.unwrap();
        assert!(list_keys(&store).await.unwrap().is_empty());
    }
}
