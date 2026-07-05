//! Endpoint API keys gating the local router. The `key` column is encrypted
//! at rest via [`crate::llm_router::secrets`]; the config-apply feature still
//! gets the literal plaintext back in-memory (from [`create_key`]'s return
//! value and from decrypt-on-read in [`row_to_key`]) so it can write the key
//! into agent configs.
use crate::llm_router::secrets;
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
    let stored: String = r.get(2)?;
    // decrypt_field passes non-`enc:`-prefixed values through, so legacy
    // plaintext rows (pre-F3b) still round-trip here.
    let key = secrets::decrypt_field(&stored).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            2,
            rusqlite::types::Type::Text,
            Box::<dyn std::error::Error + Send + Sync>::from(e.to_string()),
        )
    })?;
    Ok(EndpointKey {
        id: r.get(0)?,
        name: r.get(1)?,
        key,
        created_at: r.get(3)?,
        last_used_at: r.get(4)?,
    })
}

pub async fn list_keys(store: &Store) -> anyhow::Result<Vec<EndpointKey>> {
    store
        .with_conn(|c| -> rusqlite::Result<Vec<EndpointKey>> {
            let mut stmt = c.prepare(&format!(
                "SELECT {COLS} FROM endpoint_keys ORDER BY created_at ASC"
            ))?;
            let items = stmt
                .query_map([], row_to_key)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
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
            let encrypted = secrets::encrypt_field(&row.key);
            c.execute(
                "INSERT INTO endpoint_keys(id,name,key,created_at,last_used_at) VALUES (?1,?2,?3,?4,NULL)",
                params![row.id, row.name, encrypted, row.created_at],
            )
            .map(|_| ())
        })
        .await?;
    // Return the plaintext key so the UI can display it once; every later
    // read (list_keys/first_key) goes through row_to_key, which decrypts.
    Ok(k)
}

pub async fn revoke_key(store: &Store, id: &str) -> anyhow::Result<()> {
    let id = id.to_string();
    store
        .with_conn(move |c| {
            c.execute("DELETE FROM endpoint_keys WHERE id=?1", params![id])
                .map(|_| ())
        })
        .await
}

/// True when `presented` matches a stored key; bumps `last_used_at`.
///
/// Keys are encrypted at rest with a random nonce per write, so the same
/// plaintext never encrypts to the same ciphertext twice — a `WHERE
/// key=?1` match against the encrypted column would never hit. Instead this
/// selects every row and decrypts-and-compares in Rust.
pub async fn verify_key(store: &Store, presented: &str) -> anyhow::Result<bool> {
    let presented = presented.to_string();
    store
        .with_conn(move |c| -> rusqlite::Result<bool> {
            let mut stmt = c.prepare("SELECT id, key FROM endpoint_keys")?;
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            for row in rows {
                let (id, stored) = row?;
                // decrypt_field passes non-`enc:`-prefixed values through, so
                // legacy plaintext keys (pre-F3b) still match.
                let plain = secrets::decrypt_field(&stored).unwrap_or(stored);
                if plain == presented {
                    c.execute(
                        "UPDATE endpoint_keys SET last_used_at=?2 WHERE id=?1",
                        params![id, now_ms()],
                    )?;
                    return Ok(true);
                }
            }
            Ok(false)
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

    #[tokio::test]
    async fn verify_key_authenticates_after_encryption() {
        let store = mem_store().await;
        let k = create_key(&store, "dev").await.unwrap();

        assert!(verify_key(&store, &k.key).await.unwrap());
        assert!(!verify_key(&store, "ryz-definitely-wrong").await.unwrap());

        let listed = list_keys(&store).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert!(
            listed[0].last_used_at.is_some(),
            "successful verify must bump last_used_at"
        );
    }

    #[tokio::test]
    async fn at_rest_key_is_ciphertext() {
        let store = mem_store().await;
        let k = create_key(&store, "dev").await.unwrap();

        let raw: String = store
            .with_conn(move |c| {
                c.query_row(
                    "SELECT key FROM endpoint_keys WHERE id=?1",
                    params![k.id.clone()],
                    |r| r.get(0),
                )
            })
            .await
            .unwrap();

        assert!(
            raw.starts_with("enc:v1:"),
            "expected ciphertext prefix, got {raw}"
        );
        assert_ne!(raw, k.key);
    }

    #[tokio::test]
    async fn verify_key_matches_legacy_plaintext_row() {
        let store = mem_store().await;
        let plaintext = "ryz-legacy-plaintext-key";

        // Simulate a pre-F3b row written before encryption existed: the key
        // column holds raw plaintext, not an `enc:v1:` blob.
        let id = new_id();
        let plaintext_owned = plaintext.to_string();
        store
            .with_conn(move |c| {
                c.execute(
                    "INSERT INTO endpoint_keys(id,name,key,created_at,last_used_at) VALUES (?1,?2,?3,?4,NULL)",
                    params![id, "legacy", plaintext_owned, now_ms()],
                )
                .map(|_| ())
            })
            .await
            .unwrap();

        assert!(verify_key(&store, plaintext).await.unwrap());
    }
}
