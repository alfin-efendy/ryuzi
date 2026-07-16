#[cfg(test)]
use crate::agents::bootstrap::AgentPersistence;
#[cfg(test)]
use crate::agents::types::AgentMutationInput;
use crate::api::types::AgentMention;
use std::collections::HashSet;
use std::future::Future;
use std::sync::Arc;

pub const COORDINATOR_SYNTHESIS_INSTRUCTION: &str = "The user explicitly assigned this task to the delegated main agents below. Do not redo their task. Synthesize one answer from every result, identify each agent by name, and state every partial failure explicitly.";

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedMentions {
    pub task: String,
    pub target_agent_ids: Vec<String>,
    pub targets: Vec<Arc<crate::agents::types::AgentSnapshot>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MentionError {
    InvalidSpan { reason: String },
    StaleLabel { agent_id: String },
    Primary { agent_id: String },
    Unknown { agent_id: String },
    NonExecutable { agent_id: String },
    EmptyTask,
}

impl std::fmt::Display for MentionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSpan { reason } => write!(f, "invalid mention span: {reason}"),
            Self::StaleLabel { agent_id } => {
                write!(f, "stale mention label for agent `{agent_id}`")
            }
            Self::Primary { agent_id } => {
                write!(f, "mention target `{agent_id}` is the primary agent")
            }
            Self::Unknown { agent_id } => write!(f, "unknown mention target `{agent_id}`"),
            Self::NonExecutable { agent_id } => {
                write!(f, "mention target `{agent_id}` is not executable")
            }
            Self::EmptyTask => f.write_str("empty delegated task"),
        }
    }
}

impl std::error::Error for MentionError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoordinatorOutcome {
    pub agent_id: String,
    pub agent_name: String,
    pub task: String,
    pub status: String,
    pub result: Option<String>,
    pub error: Option<String>,
}

pub fn coordinator_context(outcomes: &[CoordinatorOutcome]) -> String {
    outcomes
        .iter()
        .map(coordinator_outcome_context)
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn coordinator_outcome_context(outcome: &CoordinatorOutcome) -> String {
    format!(
        "Agent: {}\nTask: {}\nStatus: {}\nResult: {}\nError: {}",
        outcome.agent_name,
        outcome.task,
        outcome.status,
        outcome.result.as_deref().unwrap_or(""),
        outcome.error.as_deref().unwrap_or(""),
    )
}

pub async fn coordinator_context_from_run(
    store: &crate::store::Store,
    session_pk: &str,
    root_run_id: &str,
) -> anyhow::Result<String> {
    let outcomes = store
        .list_run_messages(session_pk, root_run_id)
        .await?
        .into_iter()
        .filter(|message| message.role == "system" && message.block_type == "coordinator_outcome")
        .map(|message| CoordinatorOutcome {
            agent_id: message.payload["agent_id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            agent_name: message.payload["name"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            task: message.payload["task"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            status: message.payload["status"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            result: message.payload["result"].as_str().map(str::to_string),
            error: message.payload["error"].as_str().map(str::to_string),
        })
        .collect::<Vec<_>>();
    Ok(coordinator_context(&outcomes))
}

pub async fn coordinate_explicit_mentions<
    Queue,
    QueueFuture,
    Await,
    AwaitFuture,
    Synthesize,
    SynthesizeFuture,
>(
    resolved: &ResolvedMentions,
    mut queue: Queue,
    mut await_outcome: Await,
    mut synthesize: Synthesize,
) -> anyhow::Result<()>
where
    Queue: FnMut(Arc<crate::agents::types::AgentSnapshot>, String) -> QueueFuture,
    QueueFuture: Future<Output = anyhow::Result<String>>,
    Await: FnMut(String) -> AwaitFuture,
    AwaitFuture: Future<Output = anyhow::Result<CoordinatorOutcome>>,
    Synthesize: FnMut(String) -> SynthesizeFuture,
    SynthesizeFuture: Future<Output = anyhow::Result<()>>,
{
    let queued = futures::future::join_all(resolved.targets.iter().cloned().map(|target| {
        let task = resolved.task.clone();
        queue(target, task)
    }))
    .await;
    let run_ids = queued.into_iter().collect::<anyhow::Result<Vec<_>>>()?;
    let outcomes = futures::future::join_all(run_ids.into_iter().map(&mut await_outcome))
        .await
        .into_iter()
        .collect::<anyhow::Result<Vec<_>>>()?;
    let context = coordinator_context(&outcomes);
    synthesize(format!("{COORDINATOR_SYNTHESIS_INSTRUCTION}\n\n{context}")).await
}

pub async fn resolve_mentions(
    text: &str,
    mentions: &[AgentMention],
    primary_agent_id: &str,
    registry: &crate::agents::registry::AgentRegistry,
) -> Result<ResolvedMentions, MentionError> {
    let mut boundaries = vec![Some(0usize)];
    for (byte, character) in text.char_indices() {
        if byte != 0 {
            boundaries.push(Some(byte));
        }
        for _ in 1..character.len_utf16() {
            boundaries.push(None);
        }
    }
    boundaries.push(Some(text.len()));
    let boundary_at = |offset| boundaries.get(offset as usize).copied().flatten();

    let mut spans = Vec::with_capacity(mentions.len());
    for mention in mentions {
        if mention.start_utf16 >= mention.end_utf16 {
            return Err(MentionError::InvalidSpan {
                reason: "range is empty or reversed".into(),
            });
        }
        let Some(start) = boundary_at(mention.start_utf16) else {
            return Err(MentionError::InvalidSpan {
                reason: "start is out of bounds or splits a UTF-16 surrogate pair".into(),
            });
        };
        let Some(end) = boundary_at(mention.end_utf16) else {
            return Err(MentionError::InvalidSpan {
                reason: "end is out of bounds or splits a UTF-16 surrogate pair".into(),
            });
        };
        let token = format!("@{}", mention.label_snapshot);
        if text[start..end] != token {
            return Err(MentionError::StaleLabel {
                agent_id: mention.agent_id.clone(),
            });
        }
        spans.push((start, end, mention));
    }

    let mut ordered = spans.clone();
    ordered.sort_by_key(|(start, end, _)| (*start, *end));
    for pair in ordered.windows(2) {
        if pair[0].1 > pair[1].0 {
            return Err(MentionError::InvalidSpan {
                reason: "ranges overlap".into(),
            });
        }
    }

    let mut seen = HashSet::new();
    let mut target_agent_ids = Vec::new();
    let mut targets = Vec::new();
    for (_, _, mention) in &spans {
        if !seen.insert(mention.agent_id.clone()) {
            continue;
        }
        if mention.agent_id == primary_agent_id {
            return Err(MentionError::Primary {
                agent_id: mention.agent_id.clone(),
            });
        }
        let target = registry
            .resolved_snapshot(&mention.agent_id)
            .await
            .map_err(|_| MentionError::Unknown {
                agent_id: mention.agent_id.clone(),
            })?;
        if !target.executable {
            return Err(MentionError::NonExecutable {
                agent_id: mention.agent_id.clone(),
            });
        }
        target_agent_ids.push(mention.agent_id.clone());
        targets.push(target);
    }

    let mut task = text.to_string();
    for (start, end, _) in ordered.into_iter().rev() {
        task.replace_range(start..end, "");
    }
    if task.trim().is_empty() {
        return Err(MentionError::EmptyTask);
    }
    Ok(ResolvedMentions {
        task,
        target_agent_ids,
        targets,
    })
}

#[cfg(test)]
fn m(agent_id: &str, label: &str, start_utf16: u32, end_utf16: u32) -> AgentMention {
    AgentMention {
        agent_id: agent_id.into(),
        label_snapshot: label.into(),
        start_utf16,
        end_utf16,
    }
}

#[cfg(test)]
fn utf16_span(text: &str, token: &str, occurrence: usize) -> (u32, u32) {
    let byte_start = text
        .match_indices(token)
        .nth(occurrence)
        .expect("token occurrence")
        .0;
    let start = text[..byte_start].encode_utf16().count() as u32;
    (start, start + token.encode_utf16().count() as u32)
}

#[cfg(test)]
async fn registry() -> (
    crate::agents::bootstrap::AgentPersistence,
    String,
    String,
    String,
) {
    let database = tempfile::NamedTempFile::new().unwrap();
    let store = Arc::new(crate::store::Store::open(database.path()).await.unwrap());
    crate::llm_router::connections::add_connection(
        &store,
        crate::llm_router::connections::ConnectionRow {
            id: "test-anthropic".into(),
            provider: "anthropic".into(),
            auth_type: "api_key".into(),
            label: "Test Anthropic".into(),
            priority: 0,
            enabled: true,
            data: crate::llm_router::connections::ConnectionData {
                api_key: Some("test-key".into()),
                models_override: Some(vec!["claude-opus-4-8".into()]),
                ..Default::default()
            },
            created_at: 0,
            updated_at: 0,
        },
    )
    .await
    .unwrap();
    crate::agents::bootstrap::ensure_default_routes(&store)
        .await
        .unwrap();
    let persistence = AgentPersistence::temporary(store).await.unwrap();
    let primary = persistence.registry.default_agent_id().await;
    let template = persistence
        .registry
        .resolved_snapshot(&primary)
        .await
        .unwrap()
        .profile
        .clone();
    let input = |name: &str| AgentMutationInput {
        name: name.into(),
        description: template.description.clone(),
        avatar: template.avatar.clone(),
        model: template.model.clone(),
        permissions: template.permissions.clone(),
        skills: template.skills.clone(),
        tools: template.tools.clone(),
        loop_settings: template.loop_settings.clone(),
    };
    let ada = persistence.registry.create(input("Ada")).await.unwrap();
    let bob = persistence.registry.create(input("Bob")).await.unwrap();
    (persistence, primary, ada.profile.id, bob.profile.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mentions_use_utf16_offsets_dedupe_ids_and_preserve_other_text() {
        let (persistence, primary, ada, bob) = registry().await;
        let text = "😀 ask @Ada, then @Bob and @Ada about café";
        let ada_one = utf16_span(text, "@Ada", 0);
        let bob_span = utf16_span(text, "@Bob", 0);
        let ada_two = utf16_span(text, "@Ada", 1);
        let mentions = vec![
            m(&ada, "Ada", ada_one.0, ada_one.1),
            m(&bob, "Bob", bob_span.0, bob_span.1),
            m(&ada, "Ada", ada_two.0, ada_two.1),
        ];

        let got = resolve_mentions(text, &mentions, &primary, &persistence.registry)
            .await
            .unwrap();

        assert_eq!(got.target_agent_ids, vec![ada, bob]);
        assert_eq!(got.task, "😀 ask , then  and  about café");
    }

    #[tokio::test]
    async fn mentions_validate_submitted_spans_in_order_but_retain_first_unique_target_order() {
        let (persistence, primary, ada, bob) = registry().await;
        let text = "@Ada and @Bob";
        let ada_span = utf16_span(text, "@Ada", 0);
        let bob_span = utf16_span(text, "@Bob", 0);
        let mentions = vec![
            m(&bob, "Bob", bob_span.0, bob_span.1),
            m(&ada, "Ada", ada_span.0, ada_span.1),
        ];

        let got = resolve_mentions(text, &mentions, &primary, &persistence.registry)
            .await
            .unwrap();

        assert_eq!(got.target_agent_ids, vec![bob, ada]);
        assert_eq!(got.task, " and ");
    }

    #[tokio::test]
    async fn mentions_reject_spoofed_stale_labels_and_invalid_utf16_ranges() {
        let (persistence, primary, ada, _bob) = registry().await;
        let text = "😀 @Ada @Bob";
        let ada_span = utf16_span(text, "@Ada", 0);
        let bob_span = utf16_span(text, "@Bob", 0);
        for mentions in [
            vec![m(&ada, "Bob", ada_span.0, ada_span.1)],
            vec![m(&ada, "Ada", bob_span.0, bob_span.1)],
            vec![m(&ada, "Ada", ada_span.0, bob_span.1)],
            vec![m(&ada, "Ada", 1, ada_span.1)],
            vec![m(&ada, "Ada", 0, 99)],
            vec![m(&ada, "Ada", ada_span.0, ada_span.0)],
        ] {
            let error = resolve_mentions(text, &mentions, &primary, &persistence.registry)
                .await
                .unwrap_err();
            assert!(matches!(
                error,
                MentionError::InvalidSpan { .. } | MentionError::StaleLabel { .. }
            ));
        }
    }

    #[tokio::test]
    async fn mentions_reject_overlap_primary_unknown_and_empty_delegated_task() {
        let (persistence, primary, ada, _bob) = registry().await;
        let primary_label = persistence
            .registry
            .resolved_snapshot(&primary)
            .await
            .unwrap()
            .profile
            .name
            .clone();
        let primary_text = format!("@{primary_label}");
        let primary_span = utf16_span(&primary_text, &primary_text, 0);
        let text = "@Ada";
        let span = utf16_span(text, "@Ada", 0);
        for (candidate_text, mentions, expected) in [
            (
                text,
                vec![
                    m(&ada, "Ada", span.0, span.1),
                    m(&ada, "Ada", span.0, span.1),
                ],
                "overlap",
            ),
            (
                primary_text.as_str(),
                vec![m(&primary, &primary_label, primary_span.0, primary_span.1)],
                "primary",
            ),
            (text, vec![m("missing", "Ada", span.0, span.1)], "unknown"),
            (text, vec![m(&ada, "Ada", span.0, span.1)], "empty"),
        ] {
            let error =
                resolve_mentions(candidate_text, &mentions, &primary, &persistence.registry)
                    .await
                    .unwrap_err();
            assert!(error.to_string().contains(expected), "{error}");
        }
    }

    #[tokio::test]
    async fn mentions_reject_nonexecutable_targets() {
        let database = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(crate::store::Store::open(database.path()).await.unwrap());
        crate::agents::bootstrap::ensure_default_routes(&store)
            .await
            .unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("agents/broken")).unwrap();
        std::fs::write(
            root.path().join("agents/index.yaml"),
            "schema_version: 1\norder: [broken]\ndefault_agent_id: broken\n",
        )
        .unwrap();
        std::fs::write(
            root.path().join("agents/subagents.yaml"),
            "schema_version: 1\nmodel: { name: anthropic/claude-opus-4-8 }\n",
        )
        .unwrap();
        std::fs::write(
            root.path().join("agents/broken/agent.yaml"),
            "schema_version: 1\nid: broken\nname: Broken\ndescription: Broken.\navatar: { color: violet }\nmodel: { route: missing-route }\npermissions: { mode: ask, rules: [] }\nskills: { enabled: [] }\ntools: { native: [read], plugins: [], apps: [] }\nloop: { max_turns: 50, max_tool_rounds: 100 }\n",
        )
        .unwrap();
        let registry = crate::agents::registry::AgentRegistry::load(root.path().into(), store)
            .await
            .unwrap();
        let text = "@Broken task";
        let span = utf16_span(text, "@Broken", 0);
        let error = resolve_mentions(
            text,
            &[m("broken", "Broken", span.0, span.1)],
            "primary",
            &registry,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, MentionError::NonExecutable { .. }));
    }

    #[tokio::test]
    async fn coordinator_context_reads_structured_terminal_entries_in_run_order() {
        let database = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(crate::store::Store::open(database.path()).await.unwrap());
        let primary = "primary".to_string();
        let root = crate::domain::NewAgentRun {
            run_id: "root".into(),
            session_pk: "session".into(),
            parent_run_id: None,
            retry_of: None,
            primary_agent_id: primary,
            executing_agent_id: Some("primary".into()),
            executing_agent_name_snapshot: "Primary".into(),
            agent_kind: crate::domain::AgentRunKind::Primary,
            task: "coordinate".into(),
            status: crate::domain::AgentRunStatus::Queued,
            resolved_model: None,
            resolved_effort: None,
        };
        store
            .with_conn(move |connection| {
                connection.execute(
                    "INSERT INTO sessions(session_pk,status,perm_mode,kind,branch_owned,resume_attempts) VALUES ('session','idle','default','chat',0,0)",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        store.insert_primary_agent_run(root).await.unwrap();
        for (name, task, status, result, error) in [
            ("Ada", "inspect", "completed", Some("approved"), None),
            ("Bob", "test", "cancelled", None, Some("cancelled")),
        ] {
            store
                .insert_run_message(
                    "root",
                    crate::domain::NewMessage::block(
                        "session",
                        "system",
                        "coordinator_outcome",
                        serde_json::json!({
                            "name": name,
                            "task": task,
                            "status": status,
                            "result": result,
                            "error": error,
                        }),
                    ),
                )
                .await
                .unwrap();
        }

        let context = coordinator_context_from_run(&store, "session", "root")
            .await
            .unwrap();

        assert_eq!(
            context,
            "Agent: Ada\nTask: inspect\nStatus: completed\nResult: approved\nError: \n\nAgent: Bob\nTask: test\nStatus: cancelled\nResult: \nError: cancelled"
        );
    }

    #[tokio::test]
    async fn explicit_mentions_queue_all_targets_concurrently_and_synthesize_once_with_partial_failures(
    ) {
        let (persistence, primary, ada, bob) = registry().await;
        let text = "@Ada@Bob@Ada review the change";
        let ada_span = utf16_span(text, "@Ada", 0);
        let bob_span = utf16_span(text, "@Bob", 0);
        let duplicate_ada_span = utf16_span(text, "@Ada", 1);
        let resolved = resolve_mentions(
            text,
            &[
                m(&ada, "Ada", ada_span.0, ada_span.1),
                m(&bob, "Bob", bob_span.0, bob_span.1),
                m(&ada, "Ada", duplicate_ada_span.0, duplicate_ada_span.1),
            ],
            &primary,
            &persistence.registry,
        )
        .await
        .unwrap();
        assert_eq!(resolved.target_agent_ids, vec![ada.clone(), bob.clone()]);
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let synthesis = Arc::new(tokio::sync::Mutex::new(Vec::new()));

        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            coordinate_explicit_mentions(
                &resolved,
                {
                    let barrier = Arc::clone(&barrier);
                    move |target, _task| {
                        let barrier = Arc::clone(&barrier);
                        async move {
                            barrier.wait().await;
                            Ok(target.profile.id.clone())
                        }
                    }
                },
                {
                    let ada = ada.clone();
                    let bob = bob.clone();
                    move |run_id| {
                        let ada = ada.clone();
                        let bob = bob.clone();
                        async move {
                            Ok(CoordinatorOutcome {
                                agent_id: run_id.clone(),
                                agent_name: if run_id == ada { "Ada" } else { "Bob" }.into(),
                                task: "review the change".into(),
                                status: if run_id == ada { "completed" } else { "failed" }.into(),
                                result: (run_id == ada).then_some("approved".into()),
                                error: (run_id == bob).then_some("timed out".into()),
                            })
                        }
                    }
                },
                {
                    let synthesis = Arc::clone(&synthesis);
                    move |prompt| {
                        let synthesis = Arc::clone(&synthesis);
                        async move {
                            synthesis.lock().await.push(prompt);
                            Ok(())
                        }
                    }
                },
            ),
        )
        .await
        .expect("unique targets must be queued concurrently")
        .unwrap();

        let synthesis = synthesis.lock().await;
        assert_eq!(synthesis.len(), 1);
        assert_eq!(
            synthesis[0],
            format!(
                "{COORDINATOR_SYNTHESIS_INSTRUCTION}\n\nAgent: Ada\nTask: review the change\nStatus: completed\nResult: approved\nError: \n\nAgent: Bob\nTask: review the change\nStatus: failed\nResult: \nError: timed out"
            )
        );
    }
}
