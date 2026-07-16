use crate::harness::native::agents::ToolFilter;
use crate::harness::native::capabilities::{
    ToolCapabilityProfile, ToolInteractionMode, CAPABILITY_SCHEMA_VERSION,
};
use crate::harness::native::tool_contract::{
    compile_canonical_schema, compile_openai_strict_schema, ToolDescriptor,
};
use crate::harness::native::tools::{RegisteredTool, ToolRegistry};
use crate::store::Store;
pub use crate::store::StoredNativeToolPlan;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const SESSION_TOOL_PLAN_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolPlanLimits {
    pub schema_budget_tokens: u32,
    pub estimated_schema_tokens: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedTool {
    pub canonical_name: String,
    pub descriptor: ToolDescriptor,
    pub canonical_schema: Value,
    pub wire_schema: Value,
    pub strict: bool,
    pub contract_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionToolPlanBody {
    pub schema_version: u32,
    pub capability_profile: ToolCapabilityProfile,
    pub registry_generation: u64,
    pub canonical_tools: Vec<PlannedTool>,
    pub visible_definitions: Vec<Value>,
    pub deferred_catalog: Vec<Value>,
    pub policy_aliases: BTreeMap<String, String>,
    pub limits: ToolPlanLimits,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionToolPlan {
    pub body: SessionToolPlanBody,
    pub plan_hash: String,
    pub canonical_json: String,
}

impl SessionToolPlan {
    pub fn from_body(body: SessionToolPlanBody) -> anyhow::Result<Self> {
        let canonical_json = canonical_json(&serde_json::to_value(&body)?)?;
        let plan_hash = format!("{:x}", Sha256::digest(canonical_json.as_bytes()));
        Ok(Self {
            body,
            plan_hash,
            canonical_json,
        })
    }
}

impl AsRef<SessionToolPlan> for SessionToolPlan {
    fn as_ref(&self) -> &SessionToolPlan {
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledSessionToolPlan {
    pub plan: SessionToolPlan,
    pub canonical_tools: BTreeMap<String, PlannedTool>,
    pub visible_definitions: Vec<Value>,
}

impl AsRef<SessionToolPlan> for CompiledSessionToolPlan {
    fn as_ref(&self) -> &SessionToolPlan {
        &self.plan
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolPlanError {
    pub code: &'static str,
    pub message: String,
    pub details: Value,
}

impl ToolPlanError {
    fn new(code: &'static str, message: impl Into<String>, details: Value) -> Self {
        Self {
            code,
            message: message.into(),
            details,
        }
    }

    fn unavailable(message: impl Into<String>) -> Self {
        Self::new("capability_unavailable", message, Value::Object(Map::new()))
    }

    fn invalid_persisted(message: impl Into<String>) -> Self {
        Self::new(
            "invalid_persisted_tool_plan",
            message,
            Value::Object(Map::new()),
        )
    }

    fn store(error: anyhow::Error) -> Self {
        Self::new(
            "tool_plan_store_error",
            error.to_string(),
            Value::Object(Map::new()),
        )
    }
}

impl std::fmt::Display for ToolPlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ToolPlanError {}

pub fn canonical_json(value: &Value) -> anyhow::Result<String> {
    serde_json::to_string(&sort_json(value))
        .map_err(|error| anyhow::anyhow!("failed to serialize canonical tool plan: {error}"))
}

fn sort_json(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(sort_json).collect()),
        Value::Object(object) => {
            let mut entries = object.iter().collect::<Vec<_>>();
            entries.sort_by_key(|(left, _)| *left);
            let mut sorted = Map::new();
            for (key, value) in entries {
                sorted.insert(key.clone(), sort_json(value));
            }
            Value::Object(sorted)
        }
        scalar => scalar.clone(),
    }
}

pub async fn compile_candidate(
    registry: &ToolRegistry,
    policy: &ToolFilter,
    capability_profile: ToolCapabilityProfile,
    review_tool_defs: Option<&[Value]>,
) -> Result<CompiledSessionToolPlan, ToolPlanError> {
    if capability_profile.capability_schema_version != CAPABILITY_SCHEMA_VERSION {
        return Err(ToolPlanError::unavailable(
            "unsupported tool capability profile version",
        ));
    }
    if capability_profile.interaction_mode == ToolInteractionMode::CodeOrchestrator {
        return Err(ToolPlanError::unavailable(
            "direct function tools are unavailable for this capability profile",
        ));
    }

    let review_by_name = review_tool_defs.map(index_review_definitions).transpose()?;
    let mut canonical_names = BTreeSet::new();
    let mut legacy_names = BTreeMap::<String, BTreeSet<String>>::new();
    for legacy_name in registry.names() {
        let Some(tool) = registry.get(&legacy_name) else {
            continue;
        };
        let canonical_name = tool.descriptor().canonical_name;
        legacy_names
            .entry(canonical_name.clone())
            .or_default()
            .insert(legacy_name);
        canonical_names.insert(canonical_name);
    }

    let selected_names = match &review_by_name {
        Some(review) => review.keys().cloned().collect::<BTreeSet<_>>(),
        None => canonical_names,
    };
    let mut planned_tools = Vec::new();
    let mut visible_definitions = Vec::new();
    let mut policy_aliases = BTreeMap::new();

    for canonical_name in selected_names {
        let Some(registered) = registry.registered(&canonical_name) else {
            if review_by_name.is_some() {
                return Err(ToolPlanError::unavailable(format!(
                    "captured review tool {canonical_name} is no longer registered"
                )));
            }
            continue;
        };
        if registered.descriptor.v1_only
            || !registered.v2_schema_eligible
            || registered.descriptor.kind == "internal"
        {
            if review_by_name.is_some() {
                return Err(ToolPlanError::unavailable(format!(
                    "captured review tool {canonical_name} is not V2 eligible"
                )));
            }
            continue;
        }
        if review_by_name.is_none()
            && !policy_allows(
                policy,
                &registered.descriptor,
                legacy_names.get(&canonical_name),
            )
        {
            continue;
        }
        if registry.available(&canonical_name).await.is_err() {
            if review_by_name.is_some() {
                return Err(ToolPlanError::unavailable(format!(
                    "captured review tool {canonical_name} is unavailable"
                )));
            }
            continue;
        }

        let (wire_schema, strict) = selected_wire_schema(&registered, &capability_profile);
        let definition = definition_for(&registered, &capability_profile, &wire_schema, strict);
        if let Some(review) = &review_by_name {
            let captured = review
                .get(&canonical_name)
                .expect("selected review name came from the indexed definitions");
            if serde_json::to_vec(captured).ok() != serde_json::to_vec(&definition).ok() {
                return Err(ToolPlanError::unavailable(format!(
                    "captured review tool {canonical_name} no longer matches the registry contract"
                )));
            }
            visible_definitions.push((*captured).clone());
        } else {
            visible_definitions.push(definition);
        }

        let mut descriptor = registered.descriptor.clone();
        descriptor.input_schema = registered.canonical_schema.clone();
        for alias in &descriptor.policy_aliases {
            policy_aliases.insert(alias.clone(), canonical_name.clone());
        }
        let planned = PlannedTool {
            canonical_name: canonical_name.clone(),
            contract_hash: planned_contract_hash(
                &descriptor,
                &registered.canonical_schema,
                &wire_schema,
                strict,
            )
            .map_err(|error| ToolPlanError::unavailable(error.to_string()))?,
            descriptor,
            canonical_schema: registered.canonical_schema.clone(),
            wire_schema,
            strict,
        };
        planned_tools.push(planned);
    }

    visible_definitions.sort_by(|left, right| definition_name(left).cmp(definition_name(right)));
    planned_tools.sort_by(|left, right| left.canonical_name.cmp(&right.canonical_name));
    let definition_bytes = serde_json::to_vec(&visible_definitions).map_err(|error| {
        ToolPlanError::unavailable(format!("tool definitions cannot be serialized: {error}"))
    })?;
    let estimated_schema_tokens = definition_bytes.len().div_ceil(4) as u32;
    if estimated_schema_tokens > capability_profile.schema_budget_tokens {
        return Err(ToolPlanError::new(
            "schema_budget_exceeded",
            "visible tool definitions exceed the schema budget",
            serde_json::json!({
                "tool_count": visible_definitions.len(),
                "estimated_tokens": estimated_schema_tokens,
                "budget": capability_profile.schema_budget_tokens,
            }),
        ));
    }

    let plan = SessionToolPlan::from_body(SessionToolPlanBody {
        schema_version: SESSION_TOOL_PLAN_SCHEMA_VERSION,
        registry_generation: registry.generation(),
        limits: ToolPlanLimits {
            schema_budget_tokens: capability_profile.schema_budget_tokens,
            estimated_schema_tokens,
        },
        capability_profile,
        canonical_tools: planned_tools,
        visible_definitions,
        deferred_catalog: Vec::new(),
        policy_aliases,
    })
    .map_err(|error| ToolPlanError::unavailable(error.to_string()))?;
    compile_body(plan)
}

pub async fn load_plan(
    store: &Store,
    run_id: &str,
) -> Result<Option<CompiledSessionToolPlan>, ToolPlanError> {
    let Some(stored) = store
        .get_native_tool_plan(run_id)
        .await
        .map_err(ToolPlanError::store)?
    else {
        return Ok(None);
    };
    let body: SessionToolPlanBody = serde_json::from_str(&stored.plan_json)
        .map_err(|_| ToolPlanError::invalid_persisted("stored plan JSON is invalid"))?;
    if stored.plan_schema_version != SESSION_TOOL_PLAN_SCHEMA_VERSION
        || body.schema_version != SESSION_TOOL_PLAN_SCHEMA_VERSION
    {
        return Err(ToolPlanError::invalid_persisted(
            "stored plan schema version is unsupported",
        ));
    }
    if stored.registry_generation != body.registry_generation {
        return Err(ToolPlanError::invalid_persisted(
            "stored plan registry generation is inconsistent",
        ));
    }
    let plan = SessionToolPlan::from_body(body)
        .map_err(|_| ToolPlanError::invalid_persisted("stored plan cannot be canonicalized"))?;
    if plan.canonical_json != stored.plan_json || plan.plan_hash != stored.plan_hash {
        return Err(ToolPlanError::invalid_persisted(
            "stored plan content hash is invalid",
        ));
    }
    compile_body(plan).map(Some)
}

pub async fn freeze_plan(
    store: &Store,
    run_id: &str,
    plan: impl AsRef<SessionToolPlan>,
) -> Result<StoredNativeToolPlan, ToolPlanError> {
    let plan = plan.as_ref();
    store
        .insert_native_tool_plan(
            run_id,
            plan.body.schema_version,
            plan.body.registry_generation,
            &plan.plan_hash,
            &plan.canonical_json,
        )
        .await
        .map_err(ToolPlanError::store)
}

pub fn contract_hash_for_registered(
    registered: &RegisteredTool,
    capability_profile: &ToolCapabilityProfile,
) -> Result<String, ToolPlanError> {
    let (wire_schema, strict) = selected_wire_schema(registered, capability_profile);
    let mut descriptor = registered.descriptor.clone();
    descriptor.input_schema = registered.canonical_schema.clone();
    planned_contract_hash(
        &descriptor,
        &registered.canonical_schema,
        &wire_schema,
        strict,
    )
    .map_err(|error| ToolPlanError::unavailable(error.to_string()))
}

fn compile_body(plan: SessionToolPlan) -> Result<CompiledSessionToolPlan, ToolPlanError> {
    validate_body(&plan.body)?;
    let canonical_tools = plan
        .body
        .canonical_tools
        .iter()
        .cloned()
        .map(|tool| (tool.canonical_name.clone(), tool))
        .collect();
    let visible_definitions = plan.body.visible_definitions.clone();
    Ok(CompiledSessionToolPlan {
        plan,
        canonical_tools,
        visible_definitions,
    })
}

fn validate_body(body: &SessionToolPlanBody) -> Result<(), ToolPlanError> {
    if body.schema_version != SESSION_TOOL_PLAN_SCHEMA_VERSION
        || body.capability_profile.capability_schema_version != CAPABILITY_SCHEMA_VERSION
    {
        return Err(ToolPlanError::invalid_persisted(
            "stored plan uses an unsupported contract version",
        ));
    }
    if !body.deferred_catalog.is_empty() {
        return Err(ToolPlanError::invalid_persisted(
            "stored transitional plan has a deferred catalog",
        ));
    }
    if body.limits.schema_budget_tokens != body.capability_profile.schema_budget_tokens {
        return Err(ToolPlanError::invalid_persisted(
            "stored plan schema budget is inconsistent",
        ));
    }
    let serialized_definitions = serde_json::to_vec(&body.visible_definitions)
        .map_err(|_| ToolPlanError::invalid_persisted("stored definitions are invalid"))?;
    let estimated = serialized_definitions.len().div_ceil(4) as u32;
    if body.limits.estimated_schema_tokens != estimated
        || estimated > body.limits.schema_budget_tokens
    {
        return Err(ToolPlanError::invalid_persisted(
            "stored plan schema budget accounting is invalid",
        ));
    }
    if !is_sorted_unique(
        body.canonical_tools
            .iter()
            .map(|tool| tool.canonical_name.as_str()),
    ) || !is_sorted_unique(body.visible_definitions.iter().map(definition_name))
        || body.canonical_tools.len() != body.visible_definitions.len()
    {
        return Err(ToolPlanError::invalid_persisted(
            "stored plan definitions are not a sorted one-to-one tool set",
        ));
    }
    for (planned, definition) in body.canonical_tools.iter().zip(&body.visible_definitions) {
        if planned.canonical_name != planned.descriptor.canonical_name
            || definition_name(definition) != planned.canonical_name
            || planned.descriptor.v1_only
            || planned.descriptor.kind == "internal"
            || (planned.strict && !body.capability_profile.supports_strict_function_schema)
        {
            return Err(ToolPlanError::invalid_persisted(
                "stored tool metadata is inconsistent",
            ));
        }
        if jsonschema::validator_for(&planned.canonical_schema).is_err()
            || jsonschema::validator_for(&planned.wire_schema).is_err()
            || compile_canonical_schema(planned.canonical_schema.clone())
                != planned.canonical_schema
            || planned.descriptor.input_schema != planned.canonical_schema
        {
            return Err(ToolPlanError::invalid_persisted(
                "stored tool schema is invalid",
            ));
        }
        let expected_wire_schema = if planned.strict {
            compile_openai_strict_schema(&planned.canonical_schema).map_err(|_| {
                ToolPlanError::invalid_persisted("stored strict tool schema is invalid")
            })?
        } else {
            planned.canonical_schema.clone()
        };
        if planned.wire_schema != expected_wire_schema {
            return Err(ToolPlanError::invalid_persisted(
                "stored wire schema does not match its canonical selection",
            ));
        }
        let expected_definition = definition_from_planned(planned, &body.capability_profile);
        if &expected_definition != definition {
            return Err(ToolPlanError::invalid_persisted(
                "stored visible definition does not match its planned schema",
            ));
        }
        let expected_hash = planned_contract_hash(
            &planned.descriptor,
            &planned.canonical_schema,
            &planned.wire_schema,
            planned.strict,
        )
        .map_err(|_| ToolPlanError::invalid_persisted("stored tool contract is invalid"))?;
        if planned.contract_hash != expected_hash {
            return Err(ToolPlanError::invalid_persisted(
                "stored tool contract hash is invalid",
            ));
        }
    }
    Ok(())
}

fn is_sorted_unique<'a>(values: impl Iterator<Item = &'a str>) -> bool {
    let mut previous: Option<&str> = None;
    for value in values {
        if previous.is_some_and(|previous| previous >= value) {
            return false;
        }
        previous = Some(value);
    }
    true
}

fn policy_allows(
    policy: &ToolFilter,
    descriptor: &ToolDescriptor,
    legacy_names: Option<&BTreeSet<String>>,
) -> bool {
    policy.allows(&descriptor.canonical_name)
        || descriptor
            .policy_aliases
            .iter()
            .any(|alias| policy.allows(alias))
        || legacy_names.is_some_and(|names| names.iter().any(|name| policy.allows(name)))
}

fn selected_wire_schema(
    registered: &RegisteredTool,
    capability_profile: &ToolCapabilityProfile,
) -> (Value, bool) {
    if capability_profile.supports_strict_function_schema && registered.strict_wire_eligible {
        (
            registered
                .openai_strict_schema
                .clone()
                .expect("strict-eligible registry entries hold their compiled schema"),
            true,
        )
    } else {
        (registered.canonical_schema.clone(), false)
    }
}

fn definition_for(
    registered: &RegisteredTool,
    capability_profile: &ToolCapabilityProfile,
    wire_schema: &Value,
    strict: bool,
) -> Value {
    let mut definition = serde_json::json!({
        "name": registered.descriptor.canonical_name,
        "description": registered.descriptor.description,
        "input_schema": wire_schema,
        "strict": strict,
    });
    if capability_profile.supports_tool_output_schema {
        if let Some(output_schema) = &registered.descriptor.output_schema {
            definition["output_schema"] = output_schema.clone();
        }
    }
    definition
}

fn definition_from_planned(
    planned: &PlannedTool,
    capability_profile: &ToolCapabilityProfile,
) -> Value {
    let mut definition = serde_json::json!({
        "name": planned.canonical_name,
        "description": planned.descriptor.description,
        "input_schema": planned.wire_schema,
        "strict": planned.strict,
    });
    if capability_profile.supports_tool_output_schema {
        if let Some(output_schema) = &planned.descriptor.output_schema {
            definition["output_schema"] = output_schema.clone();
        }
    }
    definition
}

fn planned_contract_hash(
    descriptor: &ToolDescriptor,
    canonical_schema: &Value,
    wire_schema: &Value,
    strict: bool,
) -> anyhow::Result<String> {
    let contract = serde_json::json!({
        "descriptor": descriptor,
        "canonical_schema": canonical_schema,
        "wire_schema": wire_schema,
        "strict": strict,
    });
    let canonical = canonical_json(&contract)?;
    Ok(format!("{:x}", Sha256::digest(canonical.as_bytes())))
}

fn definition_name(definition: &Value) -> &str {
    definition.get("name").and_then(Value::as_str).unwrap_or("")
}

fn index_review_definitions(
    definitions: &[Value],
) -> Result<BTreeMap<String, &Value>, ToolPlanError> {
    let mut indexed = BTreeMap::new();
    for definition in definitions {
        let name = definition_name(definition);
        if name.is_empty() || indexed.insert(name.to_string(), definition).is_some() {
            return Err(ToolPlanError::unavailable(
                "captured review definitions contain an invalid or duplicate tool name",
            ));
        }
    }
    Ok(indexed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        AgentRunKind, AgentRunStatus, NewAgentRun, PermMode, Session, SessionKind, SessionStatus,
    };
    use crate::harness::native::agents::ToolFilter;
    use crate::harness::native::capabilities::{
        CapabilitySource, ToolCapabilityProfile, ToolInteractionMode, WireProtocol,
        CAPABILITY_SCHEMA_VERSION,
    };
    use crate::harness::native::tool_contract::{
        FacadePriority, ResourceScopeHint, ToolDescriptor, ToolEffect,
    };
    use crate::harness::native::tools::ToolRegistry;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn profile(schema_budget_tokens: u32) -> ToolCapabilityProfile {
        ToolCapabilityProfile {
            interaction_mode: ToolInteractionMode::DirectFunctions,
            wire_protocol: WireProtocol::OpenAiResponses,
            supports_custom_freeform_tools: false,
            supports_parallel_tool_calls: true,
            supports_strict_function_schema: true,
            supports_tool_output_schema: true,
            schema_budget_tokens,
            supports_prompt_cache: true,
            capability_source: CapabilitySource::TransportDefault,
            capability_schema_version: CAPABILITY_SCHEMA_VERSION,
        }
    }

    fn test_plan_with_schema(schema: serde_json::Value) -> SessionToolPlan {
        let descriptor = ToolDescriptor {
            canonical_name: "read".into(),
            description: "Read a file".into(),
            input_schema: schema.clone(),
            output_schema: None,
            kind: "read".into(),
            effect: ToolEffect::ReadOnly,
            idempotent: true,
            interactive: false,
            sequential_barrier: false,
            resource_scope: ResourceScopeHint::Workspace,
            result_limit_bytes: 50_000,
            facade_priority: FacadePriority::Preferred,
            policy_aliases: vec![],
            v2_only: false,
            v1_only: false,
            allow_lossless_coercions: false,
        };
        SessionToolPlan::from_body(SessionToolPlanBody {
            schema_version: SESSION_TOOL_PLAN_SCHEMA_VERSION,
            capability_profile: profile(16_000),
            registry_generation: 7,
            canonical_tools: vec![PlannedTool {
                canonical_name: "read".into(),
                descriptor,
                canonical_schema: schema.clone(),
                wire_schema: schema,
                strict: false,
                contract_hash: "contract".into(),
            }],
            visible_definitions: vec![],
            deferred_catalog: vec![],
            policy_aliases: BTreeMap::new(),
            limits: ToolPlanLimits {
                schema_budget_tokens: 16_000,
                estimated_schema_tokens: 0,
            },
        })
        .unwrap()
    }

    fn test_plan() -> SessionToolPlan {
        test_plan_with_schema(json!({"type":"object","properties":{}}))
    }

    #[test]
    fn plan_hash_is_deterministic_across_json_object_insertion_order() {
        let a = test_plan_with_schema(json!({
            "type":"object",
            "properties":{"a":{"type":"string"},"b":{"type":"integer"}}
        }));
        let b = test_plan_with_schema(json!({
            "properties":{"b":{"type":"integer"},"a":{"type":"string"}},
            "type":"object"
        }));
        assert_eq!(a.plan_hash, b.plan_hash);
        assert_eq!(a.canonical_json, b.canonical_json);
    }

    #[test]
    fn plan_contract_has_no_model_identity_field() {
        let serialized = serde_json::to_value(test_plan()).unwrap();
        assert!(serialized.get("model").is_none());
        assert!(serialized.to_string().find("terra").is_none());
    }

    #[tokio::test]
    async fn eager_plan_rejects_over_budget_schema_without_leaking_contract_content() {
        let registry = ToolRegistry::builtin();
        let error = compile_candidate(&registry, &ToolFilter::All, profile(1), None)
            .await
            .unwrap_err();

        assert_eq!(error.code, "schema_budget_exceeded");
        assert_eq!(
            error
                .details
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            ["budget", "estimated_tokens", "tool_count"]
        );
        let rendered = error.to_string();
        assert!(!rendered.contains("input_schema"));
        assert!(!rendered.contains("description"));
    }

    #[tokio::test]
    async fn eager_direct_plan_is_sorted_keeps_bash_and_has_no_deferred_loader() {
        let registry = ToolRegistry::builtin();
        let compiled = compile_candidate(&registry, &ToolFilter::All, profile(16_000), None)
            .await
            .unwrap();
        let names = compiled
            .visible_definitions
            .iter()
            .map(definition_name)
            .collect::<Vec<_>>();
        let mut sorted = names.clone();
        sorted.sort_unstable();

        assert_eq!(names, sorted);
        assert!(names.contains(&"bash"));
        assert!(!names.contains(&"load_tools"));
        assert!(compiled.plan.body.deferred_catalog.is_empty());
    }

    #[tokio::test]
    async fn non_strict_profile_uses_the_persisted_closed_canonical_schema() {
        let registry = ToolRegistry::builtin();
        let mut non_strict = profile(16_000);
        non_strict.supports_strict_function_schema = false;
        let compiled = compile_candidate(&registry, &ToolFilter::All, non_strict, None)
            .await
            .unwrap();
        let read = compiled.canonical_tools.get("read").unwrap();

        assert!(!read.strict);
        assert_eq!(read.wire_schema, read.canonical_schema);
        assert_eq!(read.canonical_schema["additionalProperties"], false);
    }

    #[tokio::test]
    async fn captured_review_definitions_require_exact_current_contract_and_stay_verbatim() {
        let registry = ToolRegistry::builtin();
        let capability_profile = profile(16_000);
        let parent = compile_candidate(
            &registry,
            &ToolFilter::All,
            capability_profile.clone(),
            None,
        )
        .await
        .unwrap();
        let captured = parent.visible_definitions.clone();
        let review = compile_candidate(
            &registry,
            &ToolFilter::Only(vec!["memory".into()]),
            capability_profile.clone(),
            Some(&captured),
        )
        .await
        .unwrap();
        assert_eq!(review.visible_definitions, captured);
        assert_eq!(review.canonical_tools.len(), captured.len());

        let mut mismatched = captured;
        mismatched[0]["description"] = Value::String("changed after capture".into());
        let error = compile_candidate(
            &registry,
            &ToolFilter::All,
            capability_profile,
            Some(&mismatched),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, "capability_unavailable");
    }

    async fn store_with_run() -> (tempfile::NamedTempFile, Store) {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        store
            .insert_session(Session {
                session_pk: "plan-session".into(),
                primary_agent_id: None,
                primary_agent_snapshot: None,
                project_id: None,
                agent_session_id: None,
                worktree_path: None,
                branch: None,
                title: Some("plan".into()),
                status: SessionStatus::Running,
                perm_mode: PermMode::Default,
                started_by: None,
                created_at: Some(1),
                last_active: Some(1),
                resume_attempts: 0,
                branch_owned: false,
                kind: SessionKind::Chat,
                speaker: None,
                agent: None,
                parent_session_pk: None,
            })
            .await
            .unwrap();
        store
            .insert_primary_agent_run(NewAgentRun {
                run_id: "plan-run".into(),
                session_pk: "plan-session".into(),
                parent_run_id: None,
                retry_of: None,
                primary_agent_id: "ada".into(),
                executing_agent_id: Some("ada".into()),
                executing_agent_name_snapshot: "Ada".into(),
                agent_kind: AgentRunKind::Primary,
                task: "plan".into(),
                status: AgentRunStatus::Queued,
                resolved_model: None,
                resolved_effort: None,
            })
            .await
            .unwrap();
        (tmp, store)
    }

    #[tokio::test]
    async fn frozen_plan_loads_exact_persisted_schemas_without_a_registry() {
        let (_tmp, store) = store_with_run().await;
        let registry = ToolRegistry::builtin();
        let candidate = compile_candidate(&registry, &ToolFilter::All, profile(16_000), None)
            .await
            .unwrap();
        freeze_plan(&store, "plan-run", &candidate).await.unwrap();
        drop(registry);

        let loaded = load_plan(&store, "plan-run").await.unwrap().unwrap();
        assert_eq!(loaded, candidate);
    }

    #[tokio::test]
    async fn load_rejects_corrupt_hash_schema_and_unsupported_version() {
        let (_tmp, store) = store_with_run().await;
        let registry = ToolRegistry::builtin();
        let candidate = compile_candidate(&registry, &ToolFilter::All, profile(16_000), None)
            .await
            .unwrap();
        let frozen = freeze_plan(&store, "plan-run", &candidate).await.unwrap();

        store
            .with_conn(|connection| {
                connection.execute(
                    "UPDATE native_tool_plans SET plan_hash='corrupt' WHERE run_id='plan-run'",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(
            load_plan(&store, "plan-run").await.unwrap_err().code,
            "invalid_persisted_tool_plan"
        );

        let original_hash = frozen.plan_hash.clone();
        let original_json = frozen.plan_json.clone();
        store
            .with_conn(move |connection| {
                connection.execute(
                    "UPDATE native_tool_plans SET plan_hash=?1,plan_json=?2 WHERE run_id='plan-run'",
                    rusqlite::params![original_hash, original_json],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let mut invalid_schema = candidate.plan.body.clone();
        invalid_schema.canonical_tools[0].canonical_schema = json!({"type": 7});
        invalid_schema.canonical_tools[0].descriptor.input_schema = json!({"type": 7});
        invalid_schema.canonical_tools[0].wire_schema = json!({"type": 7});
        invalid_schema.visible_definitions[0]["input_schema"] = json!({"type": 7});
        let invalid_schema = SessionToolPlan::from_body(invalid_schema).unwrap();
        let invalid_schema_hash = invalid_schema.plan_hash;
        let invalid_schema_json = invalid_schema.canonical_json;
        store
            .with_conn(move |connection| {
                connection.execute(
                    "UPDATE native_tool_plans SET plan_hash=?1,plan_json=?2 WHERE run_id='plan-run'",
                    rusqlite::params![invalid_schema_hash, invalid_schema_json],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(
            load_plan(&store, "plan-run").await.unwrap_err().code,
            "invalid_persisted_tool_plan"
        );

        let mut unsupported = candidate.plan.body;
        unsupported.schema_version = SESSION_TOOL_PLAN_SCHEMA_VERSION + 1;
        let unsupported = SessionToolPlan::from_body(unsupported).unwrap();
        let unsupported_hash = unsupported.plan_hash;
        let unsupported_json = unsupported.canonical_json;
        store
            .with_conn(move |connection| {
                connection.execute(
                    "UPDATE native_tool_plans SET plan_schema_version=?1,plan_hash=?2,plan_json=?3 \
                     WHERE run_id='plan-run'",
                    rusqlite::params![
                        i64::from(SESSION_TOOL_PLAN_SCHEMA_VERSION + 1),
                        unsupported_hash,
                        unsupported_json
                    ],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(
            load_plan(&store, "plan-run").await.unwrap_err().code,
            "invalid_persisted_tool_plan"
        );
    }

    #[tokio::test]
    async fn load_rejects_valid_wire_schema_that_does_not_match_canonical_selection() {
        let (_tmp, store) = store_with_run().await;
        let registry = ToolRegistry::builtin();
        let mut capability_profile = profile(16_000);
        capability_profile.supports_strict_function_schema = false;
        let candidate = compile_candidate(&registry, &ToolFilter::All, capability_profile, None)
            .await
            .unwrap();
        freeze_plan(&store, "plan-run", &candidate).await.unwrap();

        let mut inconsistent = candidate.plan.body;
        let selected = &mut inconsistent.canonical_tools[0];
        selected.wire_schema = json!({
            "type": "object",
            "properties": {"injected": {"type": "string"}},
            "additionalProperties": false
        });
        selected.contract_hash = planned_contract_hash(
            &selected.descriptor,
            &selected.canonical_schema,
            &selected.wire_schema,
            selected.strict,
        )
        .unwrap();
        inconsistent.visible_definitions[0]["input_schema"] = selected.wire_schema.clone();
        inconsistent.limits.estimated_schema_tokens =
            serde_json::to_vec(&inconsistent.visible_definitions)
                .unwrap()
                .len()
                .div_ceil(4) as u32;
        let inconsistent = SessionToolPlan::from_body(inconsistent).unwrap();
        let plan_hash = inconsistent.plan_hash;
        let plan_json = inconsistent.canonical_json;
        store
            .with_conn(move |connection| {
                connection.execute(
                    "UPDATE native_tool_plans SET plan_hash=?1,plan_json=?2 WHERE run_id='plan-run'",
                    rusqlite::params![plan_hash, plan_json],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        assert_eq!(
            load_plan(&store, "plan-run").await.unwrap_err().code,
            "invalid_persisted_tool_plan"
        );
    }
}
