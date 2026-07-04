//! Providers domain: persisted provider/account configuration (rotation,
//! failover, user-set quota limits) plus usage aggregated from the local
//! `messages` table. Token counts are estimated from persisted payload sizes
//! (~4 chars/token) — derived from real local data, labeled an estimate in
//! the UI. Provider-side quota counters have no public API.

use crate::store::Store;
use rusqlite::{params, OptionalExtension};

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderRow {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub color: String,
    pub enabled: bool,
    pub strategy: String,
    pub fail_auto: bool,
    pub threshold: u32,
    pub return_to_primary: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccountRow {
    pub id: String,
    pub provider_id: String,
    pub label: String,
    pub email: String,
    pub plan: String,
    pub sort: i64,
    pub active: bool,
    pub session_limit_tokens: Option<i64>,
    pub weekly_limit_tokens: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UsageDay {
    /// "Today" or a short weekday name ("Thu").
    pub day: String,
    pub est_tokens: f64,
}

const PROVIDER_COLS: &str =
    "id,name,kind,color,enabled,strategy,fail_auto,threshold,return_to_primary";

fn provider_from(r: &rusqlite::Row) -> rusqlite::Result<ProviderRow> {
    Ok(ProviderRow {
        id: r.get(0)?,
        name: r.get(1)?,
        kind: r.get(2)?,
        color: r.get(3)?,
        enabled: r.get::<_, i64>(4)? != 0,
        strategy: r.get(5)?,
        fail_auto: r.get::<_, i64>(6)? != 0,
        threshold: r.get::<_, i64>(7)? as u32,
        return_to_primary: r.get::<_, i64>(8)? != 0,
    })
}

const ACCOUNT_COLS: &str =
    "id,provider_id,label,email,plan,sort,active,session_limit_tokens,weekly_limit_tokens";

fn account_from(r: &rusqlite::Row) -> rusqlite::Result<AccountRow> {
    Ok(AccountRow {
        id: r.get(0)?,
        provider_id: r.get(1)?,
        label: r.get(2)?,
        email: r.get(3)?,
        plan: r.get(4)?,
        sort: r.get(5)?,
        active: r.get::<_, i64>(6)? != 0,
        session_limit_tokens: r.get(7)?,
        weekly_limit_tokens: r.get(8)?,
    })
}

pub async fn list_providers(store: &Store) -> anyhow::Result<Vec<ProviderRow>> {
    store
        .with_conn(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {PROVIDER_COLS} FROM providers ORDER BY created_at"
            ))?;
            let rows = stmt
                .query_map([], provider_from)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
}

pub async fn upsert_provider(store: &Store, p: ProviderRow) -> anyhow::Result<()> {
    let now = crate::paths::now_ms();
    store
        .with_conn(move |c| {
            c.execute(
                "INSERT INTO providers(id,name,kind,color,enabled,strategy,fail_auto,threshold,return_to_primary,created_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) \
                 ON CONFLICT(id) DO UPDATE SET \
                   name=excluded.name, kind=excluded.kind, color=excluded.color, \
                   enabled=excluded.enabled, strategy=excluded.strategy, fail_auto=excluded.fail_auto, \
                   threshold=excluded.threshold, return_to_primary=excluded.return_to_primary",
                params![
                    p.id, p.name, p.kind, p.color, p.enabled as i64, p.strategy,
                    p.fail_auto as i64, p.threshold as i64, p.return_to_primary as i64, now
                ],
            )
            .map(|_| ())
        })
        .await
}

pub async fn get_provider(store: &Store, id: &str) -> anyhow::Result<Option<ProviderRow>> {
    let id = id.to_string();
    store
        .with_conn(move |c| {
            c.query_row(
                &format!("SELECT {PROVIDER_COLS} FROM providers WHERE id=?1"),
                params![id],
                provider_from,
            )
            .optional()
        })
        .await
}

pub async fn remove_provider(store: &Store, id: &str) -> anyhow::Result<()> {
    let id = id.to_string();
    store
        .with_conn(move |c| {
            c.execute(
                "DELETE FROM provider_accounts WHERE provider_id=?1",
                params![id],
            )?;
            c.execute("DELETE FROM providers WHERE id=?1", params![id])
                .map(|_| ())
        })
        .await
}

pub async fn list_accounts(store: &Store, provider_id: &str) -> anyhow::Result<Vec<AccountRow>> {
    let provider_id = provider_id.to_string();
    store
        .with_conn(move |c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {ACCOUNT_COLS} FROM provider_accounts WHERE provider_id=?1 ORDER BY sort, id"
            ))?;
            let rows = stmt
                .query_map(params![provider_id], account_from)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
}

pub async fn add_account(store: &Store, account: AccountRow) -> anyhow::Result<()> {
    store
        .with_conn(move |c| {
            // First account for a provider becomes active automatically.
            let existing: i64 = c.query_row(
                "SELECT COUNT(*) FROM provider_accounts WHERE provider_id=?1",
                params![account.provider_id],
                |r| r.get(0),
            )?;
            let active = if existing == 0 { 1 } else { account.active as i64 };
            c.execute(
                "INSERT INTO provider_accounts(id,provider_id,label,email,plan,sort,active,session_limit_tokens,weekly_limit_tokens) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![
                    account.id, account.provider_id, account.label, account.email, account.plan,
                    existing, active, account.session_limit_tokens, account.weekly_limit_tokens
                ],
            )
            .map(|_| ())
        })
        .await
}

pub async fn remove_account(store: &Store, account_id: &str) -> anyhow::Result<()> {
    let account_id = account_id.to_string();
    store
        .with_conn(move |c| {
            c.execute(
                "DELETE FROM provider_accounts WHERE id=?1",
                params![account_id],
            )
            .map(|_| ())
        })
        .await
}

pub async fn set_active_account(
    store: &Store,
    provider_id: &str,
    account_id: &str,
) -> anyhow::Result<()> {
    let provider_id = provider_id.to_string();
    let account_id = account_id.to_string();
    store
        .with_conn(move |c| {
            c.execute(
                "UPDATE provider_accounts SET active=0 WHERE provider_id=?1",
                params![provider_id],
            )?;
            c.execute(
                "UPDATE provider_accounts SET active=1 WHERE id=?1",
                params![account_id],
            )
            .map(|_| ())
        })
        .await
}

/// Swap the account at `account_id` with its neighbor (`dir` = -1 up / +1 down).
pub async fn move_account(
    store: &Store,
    provider_id: &str,
    account_id: &str,
    dir: i32,
) -> anyhow::Result<()> {
    let accounts = list_accounts(store, provider_id).await?;
    let Some(i) = accounts.iter().position(|a| a.id == account_id) else {
        return Ok(());
    };
    let j = i as i64 + dir as i64;
    if j < 0 || j >= accounts.len() as i64 {
        return Ok(());
    }
    let (a, b) = (accounts[i].id.clone(), accounts[j as usize].id.clone());
    let (sa, sb) = (i as i64, j);
    store
        .with_conn(move |c| {
            c.execute(
                "UPDATE provider_accounts SET sort=?2 WHERE id=?1",
                params![a, sb],
            )?;
            c.execute(
                "UPDATE provider_accounts SET sort=?2 WHERE id=?1",
                params![b, sa],
            )
            .map(|_| ())
        })
        .await
}

/// Seed the default provider so the screen reflects the engine's actual
/// harness (Claude via the local CLI login) on first run.
pub async fn seed_defaults(store: &Store) -> anyhow::Result<()> {
    if !list_providers(store).await?.is_empty() {
        return Ok(());
    }
    upsert_provider(
        store,
        ProviderRow {
            id: "anthropic".into(),
            name: "Claude".into(),
            kind: "Subscription · CLI login".into(),
            color: "#D97757".into(),
            enabled: true,
            strategy: "priority".into(),
            fail_auto: false,
            threshold: 95,
            return_to_primary: true,
        },
    )
    .await
}

/// Estimated tokens (chars/4) persisted since `since_ms`, across all sessions.
pub async fn est_tokens_since(store: &Store, since_ms: i64) -> anyhow::Result<f64> {
    store
        .with_conn(move |c| {
            c.query_row(
                "SELECT COALESCE(SUM(LENGTH(payload)), 0) / 4.0 FROM messages WHERE created_at >= ?1",
                params![since_ms],
                |r| r.get::<_, f64>(0),
            )
        })
        .await
}

/// Estimated tokens per calendar day for the last `days` days (oldest first),
/// with zero-filled gaps. Labels: short weekday name, "Today" for today.
pub async fn usage_by_day(store: &Store, days: u32) -> anyhow::Result<Vec<UsageDay>> {
    let rows: Vec<(String, i64, f64)> = store
        .with_conn(move |c| {
            let mut stmt = c.prepare(
                "WITH RECURSIVE series(d, i) AS ( \
                   SELECT date('now', 'localtime'), 0 \
                   UNION ALL \
                   SELECT date('now', 'localtime', '-' || (i + 1) || ' day'), i + 1 \
                   FROM series WHERE i < ?1 - 1 \
                 ) \
                 SELECT series.d, CAST(strftime('%w', series.d) AS INTEGER), \
                        COALESCE((SELECT SUM(LENGTH(m.payload)) / 4.0 FROM messages m \
                                  WHERE date(m.created_at / 1000, 'unixepoch', 'localtime') = series.d), 0) \
                 FROM series ORDER BY series.d ASC",
            )?;
            let rows = stmt
                .query_map(params![days as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await?;

    const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let n = rows.len();
    Ok(rows
        .into_iter()
        .enumerate()
        .map(|(idx, (_date, weekday, tokens))| UsageDay {
            day: if idx + 1 == n {
                "Today".to_string()
            } else {
                WEEKDAYS[(weekday as usize).min(6)].to_string()
            },
            est_tokens: tokens,
        })
        .collect())
}

/// "4.3M", "382k", "912" — compact token formatting for quota rows.
pub fn fmt_tokens(t: f64) -> String {
    if t >= 1_000_000.0 {
        format!("{:.1}M", t / 1_000_000.0)
    } else if t >= 1_000.0 {
        format!("{:.0}k", t / 1_000.0)
    } else {
        format!("{:.0}", t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::NewMessage;

    #[tokio::test]
    async fn provider_and_account_crud_with_ordering() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();

        seed_defaults(&store).await.unwrap();
        seed_defaults(&store).await.unwrap(); // idempotent
        let providers = list_providers(&store).await.unwrap();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].id, "anthropic");

        // First account auto-activates; second stays standby.
        add_account(
            &store,
            AccountRow {
                id: "a1".into(),
                provider_id: "anthropic".into(),
                label: "Account 1".into(),
                email: "a@x.io".into(),
                plan: "Max".into(),
                sort: 0,
                active: false,
                session_limit_tokens: Some(5_000_000),
                weekly_limit_tokens: None,
            },
        )
        .await
        .unwrap();
        add_account(
            &store,
            AccountRow {
                id: "a2".into(),
                provider_id: "anthropic".into(),
                label: "Account 2".into(),
                email: "b@x.io".into(),
                plan: "Pro".into(),
                sort: 0,
                active: false,
                session_limit_tokens: None,
                weekly_limit_tokens: None,
            },
        )
        .await
        .unwrap();
        let accounts = list_accounts(&store, "anthropic").await.unwrap();
        assert!(accounts[0].active && !accounts[1].active);

        // Reorder: a2 moves up.
        move_account(&store, "anthropic", "a2", -1).await.unwrap();
        let accounts = list_accounts(&store, "anthropic").await.unwrap();
        assert_eq!(accounts[0].id, "a2");
        // Out-of-range moves are no-ops.
        move_account(&store, "anthropic", "a2", -1).await.unwrap();
        assert_eq!(
            list_accounts(&store, "anthropic").await.unwrap()[0].id,
            "a2"
        );

        set_active_account(&store, "anthropic", "a2").await.unwrap();
        let accounts = list_accounts(&store, "anthropic").await.unwrap();
        assert!(accounts.iter().find(|a| a.id == "a2").unwrap().active);
        assert!(!accounts.iter().find(|a| a.id == "a1").unwrap().active);

        remove_account(&store, "a1").await.unwrap();
        assert_eq!(list_accounts(&store, "anthropic").await.unwrap().len(), 1);

        remove_provider(&store, "anthropic").await.unwrap();
        assert!(list_providers(&store).await.unwrap().is_empty());
        assert!(list_accounts(&store, "anthropic").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn usage_aggregates_real_message_payloads() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();

        // 400 chars of payload ≈ 100 tokens (chars/4), plus JSON overhead.
        let text = "x".repeat(400);
        store
            .insert_message(NewMessage::block(
                "s1",
                "assistant",
                "text",
                serde_json::json!({ "text": text }),
            ))
            .await
            .unwrap();

        let est = est_tokens_since(&store, 0).await.unwrap();
        assert!(est >= 100.0, "expected ≥100 est tokens, got {est}");

        let days = usage_by_day(&store, 8).await.unwrap();
        assert_eq!(days.len(), 8);
        assert_eq!(days.last().unwrap().day, "Today");
        assert!(days.last().unwrap().est_tokens >= 100.0);
        // Older days are zero-filled.
        assert_eq!(days[0].est_tokens, 0.0);
    }

    #[test]
    fn formats_token_counts() {
        assert_eq!(fmt_tokens(4_300_000.0), "4.3M");
        assert_eq!(fmt_tokens(382_000.0), "382k");
        assert_eq!(fmt_tokens(912.0), "912");
    }
}
