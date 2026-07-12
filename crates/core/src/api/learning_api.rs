//! Learning screen commands (Task 11, spec §7.6): read/write persistent
//! memory, cross-session recall, the Learning panel's journey graph, curator
//! status/rollback, and skill lifecycle/pin controls. Every method here is
//! either a thin wrapper over an existing `Store`/`MemoryStore` primitive
//! (Tasks 2/4/5/10) or, for `learning_graph`, a pure assembly function
//! (`build_learning_graph`) over their outputs — no new SQL.

use super::{ok, params, ApiError};
use crate::api::types::{CuratorStatus, LearningGraph, LearningGraphEdge, LearningGraphNode};
use crate::domain::SkillUsage;
use crate::harness::native::memory::{MemoryScope, MemoryStore};
use crate::serve::ApiState;
use serde::Deserialize;
use serde_json::Value;

pub(crate) const HANDLES: &[&str] = &[
    "read_memory",
    "write_memory",
    "search_sessions",
    "learning_graph",
    "curator_status",
    "curator_rollback",
    "list_skill_usage",
    "set_skill_pinned",
];

/// `search_sessions`'s cap on returned hits — generous enough for a Learning
/// panel search box, small enough to stay a single quick round trip.
const SEARCH_LIMIT: i64 = 50;
/// `curator_status`'s cap on the run-history feed.
const CURATOR_HISTORY_LIMIT: i64 = 20;

#[derive(Deserialize)]
struct ScopeP {
    scope: String,
}

#[derive(Deserialize)]
struct WriteMemoryP {
    scope: String,
    action: String,
    text: Option<String>,
    // A raw identifier: serde's derive strips the `r#` prefix, so this reads
    // the wire key `"match"` (the same key the `memory` native tool's
    // `parse_op` reads) without a Rust keyword clash.
    r#match: Option<String>,
}

#[derive(Deserialize)]
struct QueryP {
    query: String,
}

#[derive(Deserialize)]
struct RunIdP {
    run_id: String,
}

#[derive(Deserialize)]
struct SetPinnedP {
    name: String,
    pinned: bool,
}

async fn default_memory(state: &ApiState) -> anyhow::Result<MemoryStore> {
    let agent_id = state.agents.default_agent_id().await;
    MemoryStore::for_agent(state.agent_knowledge.clone(), &agent_id, None)
}

pub(crate) async fn dispatch(state: &ApiState, method: &str, p: Value) -> Result<Value, ApiError> {
    let cp = &state.cp;
    match method {
        "read_memory" => {
            let a: ScopeP = params(p)?;
            let scope = MemoryScope::parse(&a.scope)?;
            ok(default_memory(state).await?.load(scope).await?)
        }
        // User-driven edit (the Learning panel, operated by the human) — the
        // storage layer accepts this directly with no `WriteOrigin` gate;
        // that gate exists only for autonomous tool writes (skill_manage).
        "write_memory" => {
            let a: WriteMemoryP = params(p)?;
            let scope = MemoryScope::parse(&a.scope)?;
            let store = default_memory(state).await?;
            let text = a.text.as_deref().unwrap_or("");
            let matcher = a.r#match.as_deref().unwrap_or("");
            match a.action.as_str() {
                "add" => store.add(scope, text).await?,
                "replace" => store.replace(scope, matcher, text).await?,
                "remove" => store.remove(scope, matcher).await?,
                other => {
                    return Err(ApiError::bad_request(format!(
                        "write_memory: unknown action `{other}` (add|replace|remove)"
                    )))
                }
            }
            ok(())
        }
        "search_sessions" => {
            let a: QueryP = params(p)?;
            let hits = cp
                .store()
                .search_messages_fts(&a.query, &[], SEARCH_LIMIT)
                .await
                .map_err(|e| ApiError::bad_request(e.to_string()))?;
            ok(hits)
        }
        "learning_graph" => {
            let skills = cp.store().list_skill_usage().await?;
            let mem = default_memory(state).await?;
            let global = mem.load(MemoryScope::Global).await?;
            let user = mem.load(MemoryScope::User).await?;
            ok(build_learning_graph(
                &skills,
                &[("global", &global), ("user", &user)],
            ))
        }
        "curator_status" => {
            let last_run_at = cp.store().curator_last_run().await?;
            let recent = cp.store().list_curator_runs(CURATOR_HISTORY_LIMIT).await?;
            ok(CuratorStatus {
                last_run_at,
                recent,
            })
        }
        "curator_rollback" => {
            let a: RunIdP = params(p)?;
            // No arbitrary cap here — a rollback must find the run by id
            // regardless of how far back it sits in history.
            let runs = cp.store().list_curator_runs(i64::MAX).await?;
            let run = runs
                .into_iter()
                .find(|r| r.id == a.run_id)
                .ok_or_else(|| ApiError::not_found(format!("unknown curator run: {}", a.run_id)))?;
            // The opt-in LLM consolidation pass that creates a pre-mutation
            // tar.gz snapshot (spec §7.5) is deferred (Task 10:
            // `curator.consolidate` is hard-`false`, `snapshot_path` is
            // always `None`) — so there is never yet anything to restore.
            // This stays a real, honest error (not a silent no-op) so the
            // Learning panel can surface it, and the branch below is where
            // the consolidation pass's follow-up wires the actual
            // `tar -xzf <snapshot_path> -C skills_install::skills_root()`
            // extraction once it exists.
            match run.snapshot_path {
                Some(_) => Err(ApiError::bad_request(
                    "curator rollback isn't available yet — the opt-in consolidation pass \
                     that creates a snapshot to roll back to hasn't shipped"
                        .to_string(),
                )),
                None => Err(ApiError::bad_request(format!(
                    "curator run {} has no snapshot to roll back to (only the consolidation \
                     pass creates one, and it hasn't run)",
                    run.id
                ))),
            }
        }
        "list_skill_usage" => ok(cp.store().list_skill_usage().await?),
        "set_skill_pinned" => {
            let a: SetPinnedP = params(p)?;
            cp.store().set_skill_pinned(&a.name, a.pinned).await?;
            ok(())
        }
        _ => Err(ApiError::not_found(format!("unknown method: {method}"))),
    }
}

/// Stable id for a skill node — namespaced so it can never collide with a
/// memory node's id in the same graph.
fn skill_node_id(name: &str) -> String {
    format!("skill:{name}")
}

/// Stable id for a memory-entry node: a content hash, not a positional
/// index, so a re-fetch after an unrelated edit elsewhere in the scope
/// doesn't reshuffle every later entry's id (the fragility spec §7.6 calls
/// out in Hermes' original design).
fn memory_node_id(scope: &str, text: &str) -> String {
    use sha2::{Digest, Sha256};
    let hex = format!("{:x}", Sha256::digest(text.as_bytes()));
    format!("memory:{scope}:{}", &hex[..16])
}

/// Lowercased `-`/`_`/whitespace-separated tokens of a skill name, used to
/// find lexically related skills without needing free-text descriptions
/// (`skill_usage` carries only the name).
fn name_tokens(name: &str) -> std::collections::HashSet<String> {
    name.split(|c: char| c == '-' || c == '_' || c.is_whitespace())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Trim a memory entry to a graph-node-friendly label.
fn label_for(text: &str) -> String {
    const MAX: usize = 60;
    if text.chars().count() <= MAX {
        text.to_string()
    } else {
        let head: String = text.chars().take(MAX).collect();
        format!("{head}…")
    }
}

/// Assemble the Learning panel's journey graph (spec §7.6): one node per
/// skill (`skill_usage`) and per memory entry across `memory_by_scope`, plus
/// `related_skills` edges between skills whose names share a token and
/// `lexical` edges from a memory entry to any skill its text mentions by
/// name. Pure — no I/O — so it's unit-testable without a `Store`.
pub(crate) fn build_learning_graph(
    skills: &[SkillUsage],
    memory_by_scope: &[(&str, &[String])],
) -> LearningGraph {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();

    for s in skills {
        nodes.push(LearningGraphNode {
            id: skill_node_id(&s.name),
            kind: "skill".to_string(),
            label: s.name.clone(),
            state: Some(s.state.clone()),
            scope: None,
        });
    }
    for i in 0..skills.len() {
        for j in (i + 1)..skills.len() {
            let (a, b) = (&skills[i], &skills[j]);
            if !name_tokens(&a.name).is_disjoint(&name_tokens(&b.name)) {
                edges.push(LearningGraphEdge {
                    source: skill_node_id(&a.name),
                    target: skill_node_id(&b.name),
                    kind: "related_skills".to_string(),
                });
            }
        }
    }

    for (scope, entries) in memory_by_scope {
        for text in entries.iter() {
            let id = memory_node_id(scope, text);
            nodes.push(LearningGraphNode {
                id: id.clone(),
                kind: "memory".to_string(),
                label: label_for(text),
                state: None,
                scope: Some((*scope).to_string()),
            });
            let lower = text.to_lowercase();
            for s in skills {
                if lower.contains(&s.name.to_lowercase()) {
                    edges.push(LearningGraphEdge {
                        source: id.clone(),
                        target: skill_node_id(&s.name),
                        kind: "lexical".to_string(),
                    });
                }
            }
        }
    }

    LearningGraph { nodes, edges }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{
        dispatch,
        tests_support::{state, state_with_agents},
    };
    use serde_json::json;

    /// Compile-bridge guarantee for the Plan 3 migration window: the new
    /// per-agent Learning surface and the legacy global one are both
    /// dispatched until Task 9 removes the legacy consumer and handlers.
    #[tokio::test]
    async fn per_agent_and_legacy_learning_surfaces_coexist() {
        let s = state_with_agents().await;
        let agent_id = s.agents.default_agent_id().await;
        let per_agent = dispatch(&s, "get_agent_learning", json!({"agent_id": agent_id}))
            .await
            .unwrap();
        assert!(per_agent["concepts"].is_array());
        let legacy = dispatch(&s, "read_memory", json!({"scope": "user"}))
            .await
            .unwrap();
        assert!(legacy.is_array());
    }

    fn usage(name: &str, state: &str) -> SkillUsage {
        SkillUsage {
            name: name.to_string(),
            created_by: None,
            use_count: 0,
            view_count: 0,
            patch_count: 0,
            last_used_at: None,
            last_viewed_at: None,
            last_patched_at: None,
            state: state.to_string(),
            pinned: false,
            archived_at: None,
            created_at: None,
        }
    }

    // ---------- dispatch (RPC surface), one per HANDLES method ----------

    #[tokio::test]
    async fn read_then_write_memory_roundtrips_through_dispatch() {
        let s = state().await;
        dispatch(
            &s,
            "write_memory",
            json!({
                "scope": "user", "action": "add", "text": "prefers terse answers", "match": null
            }),
        )
        .await
        .unwrap();
        let out = dispatch(&s, "read_memory", json!({ "scope": "user" }))
            .await
            .unwrap();
        assert!(out
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "prefers terse answers"));
    }

    #[tokio::test]
    async fn write_memory_rejects_an_unknown_action() {
        let s = state().await;
        let err = dispatch(
            &s,
            "write_memory",
            json!({ "scope": "global", "action": "teleport", "text": "x", "match": null }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.message.contains("unknown action"), "{}", err.message);
    }

    #[tokio::test]
    async fn search_sessions_dispatches_and_finds_a_seeded_message() {
        let s = state().await;
        s.cp.store()
            .insert_session(crate::domain::Session {
                session_pk: "chat-1".into(),
                project_id: None,
                agent_session_id: None,
                worktree_path: None,
                branch: None,
                title: Some("t-chat-1".into()),
                status: crate::domain::SessionStatus::Idle,
                perm_mode: crate::domain::PermMode::Default,
                started_by: None,
                created_at: Some(1000),
                last_active: Some(1000),
                resume_attempts: 0,
                branch_owned: false,
                kind: crate::domain::SessionKind::Chat,
                speaker: None,
                agent: None,
                parent_session_pk: None,
            })
            .await
            .unwrap();
        s.cp.store()
            .insert_message(crate::domain::NewMessage::block(
                "chat-1",
                "user",
                "text",
                json!({ "text": "kubernetes ingress routing" }),
            ))
            .await
            .unwrap();

        let out = dispatch(&s, "search_sessions", json!({ "query": "ingress" }))
            .await
            .unwrap();
        let hits = out.as_array().unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["sessionPk"], "chat-1");
    }

    #[tokio::test]
    async fn search_sessions_surfaces_a_malformed_query_as_bad_request() {
        let s = state().await;
        let err = dispatch(&s, "search_sessions", json!({ "query": "\"unterminated" }))
            .await
            .unwrap_err();
        assert_eq!(err.status, 400);
    }

    #[tokio::test]
    async fn learning_graph_dispatches_and_includes_a_skill_and_a_memory_node() {
        let s = state().await;
        s.cp.store().record_skill_use("deploy").await.unwrap();
        dispatch(
            &s,
            "write_memory",
            json!({ "scope": "global", "action": "add", "text": "uses deploy nightly", "match": null }),
        )
        .await
        .unwrap();

        let out = dispatch(&s, "learning_graph", json!({})).await.unwrap();
        let nodes = out["nodes"].as_array().unwrap();
        assert!(nodes
            .iter()
            .any(|n| n["kind"] == "skill" && n["label"] == "deploy"));
        assert!(nodes.iter().any(|n| n["kind"] == "memory"));
        let edges = out["edges"].as_array().unwrap();
        assert!(
            edges.iter().any(|e| e["kind"] == "lexical"),
            "the memory entry mentions `deploy` by name: {edges:?}"
        );
    }

    #[tokio::test]
    async fn curator_status_dispatches_and_reports_last_run_and_recent() {
        let s = state().await;
        s.cp.store()
            .insert_curator_run("run-1", 1_000)
            .await
            .unwrap();
        s.cp.store()
            .finish_curator_run("run-1", 2_000, "ok", 3, false, None, None)
            .await
            .unwrap();

        let out = dispatch(&s, "curator_status", json!({})).await.unwrap();
        assert_eq!(out["lastRunAt"], 1_000);
        let recent = out["recent"].as_array().unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0]["id"], "run-1");
    }

    #[tokio::test]
    async fn curator_rollback_unknown_run_is_not_found() {
        let s = state().await;
        let err = dispatch(&s, "curator_rollback", json!({ "run_id": "nope" }))
            .await
            .unwrap_err();
        assert_eq!(err.status, 404);
    }

    #[tokio::test]
    async fn curator_rollback_without_a_snapshot_is_a_clean_bad_request() {
        let s = state().await;
        s.cp.store()
            .insert_curator_run("run-1", 1_000)
            .await
            .unwrap();
        s.cp.store()
            .finish_curator_run("run-1", 2_000, "ok", 0, false, None, None)
            .await
            .unwrap();

        let err = dispatch(&s, "curator_rollback", json!({ "run_id": "run-1" }))
            .await
            .unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.message.contains("no snapshot"), "{}", err.message);
    }

    #[tokio::test]
    async fn list_skill_usage_dispatches_and_decodes_as_an_array() {
        let s = state().await;
        s.cp.store().record_skill_use("deploy").await.unwrap();
        let out = dispatch(&s, "list_skill_usage", json!({})).await.unwrap();
        let rows = out.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["name"], "deploy");
    }

    #[tokio::test]
    async fn set_skill_pinned_dispatches_and_persists() {
        let s = state().await;
        s.cp.store().record_skill_use("deploy").await.unwrap();
        dispatch(
            &s,
            "set_skill_pinned",
            json!({ "name": "deploy", "pinned": true }),
        )
        .await
        .unwrap();
        let usage =
            s.cp.store()
                .get_skill_usage("deploy")
                .await
                .unwrap()
                .unwrap();
        assert!(usage.pinned);
    }

    // ---------- build_learning_graph (pure) ----------

    #[test]
    fn related_skills_edge_links_skills_sharing_a_name_token() {
        let skills = vec![
            usage("skill-manage", "active"),
            usage("skill-view", "active"),
        ];
        let graph = build_learning_graph(&skills, &[]);
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].kind, "related_skills");
    }

    #[test]
    fn unrelated_skill_names_get_no_edge() {
        let skills = vec![usage("memory", "active"), usage("session_search", "active")];
        let graph = build_learning_graph(&skills, &[]);
        assert!(graph.edges.is_empty());
    }

    #[test]
    fn memory_node_ids_are_stable_content_hashes_not_positions() {
        let skills = vec![];
        let entries = vec!["fact one".to_string(), "fact two".to_string()];
        let g1 = build_learning_graph(&skills, &[("global", &entries)]);
        // Reordering the same entries must not change either node's id — a
        // content hash, not a positional index.
        let reordered = vec!["fact two".to_string(), "fact one".to_string()];
        let g2 = build_learning_graph(&skills, &[("global", &reordered)]);
        let ids1: std::collections::HashSet<_> = g1.nodes.iter().map(|n| n.id.clone()).collect();
        let ids2: std::collections::HashSet<_> = g2.nodes.iter().map(|n| n.id.clone()).collect();
        assert_eq!(ids1, ids2);
    }

    #[test]
    fn lexical_edge_links_a_memory_entry_to_the_skill_it_names() {
        let skills = vec![usage("deploy", "active")];
        let entries = vec!["remember to run deploy before lunch".to_string()];
        let graph = build_learning_graph(&skills, &[("user", &entries)]);
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].kind, "lexical");
        assert_eq!(graph.edges[0].target, "skill:deploy");
    }
}
