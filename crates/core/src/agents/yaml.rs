// Persistence scaffolding for the agent registry: several pub(crate)
// document types and renderers are only consumed by later Plan 2 tasks
// (registry state, disk writer). Until that wiring lands, suppress
// dead-code so the intermediate commits stay clippy-clean.
#![allow(dead_code)]

use anyhow::{bail, Context};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_yaml::{Mapping, Value};

use crate::agents::personality::{AgentPersonality, PersonalityPreset};

use super::types::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AgentIndexWire {
    schema_version: u32,
    order: Vec<String>,
    default_agent_id: String,
    #[serde(flatten)]
    extensions: IndexMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AvatarWire {
    color: String,
    #[serde(flatten)]
    extensions: IndexMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum AgentModelWire {
    Concrete(ConcreteModelWire),
    Route(RouteModelWire),
}

// `deny_unknown_fields` cannot be combined with `flatten` extension maps in
// serde, so union violations (both arms, or `effort` on a route) are rejected
// up front by `validate_model_union` before deserialization; the required
// `name`/`route` fields then discriminate the untagged arms deterministically.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConcreteModelWire {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    effort: Option<String>,
    #[serde(flatten)]
    extensions: IndexMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RouteModelWire {
    route: String,
    #[serde(flatten)]
    extensions: IndexMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PermissionRuleWire {
    id: String,
    tool: String,
    decision: PermissionDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    command_prefix: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PermissionsWire {
    mode: AgentPermissionMode,
    #[serde(default)]
    rules: Vec<PermissionRuleWire>,
    #[serde(flatten)]
    extensions: IndexMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SkillsWire {
    #[serde(default)]
    enabled: Vec<String>,
    #[serde(flatten)]
    extensions: IndexMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ToolsWire {
    #[serde(default)]
    native: Vec<String>,
    #[serde(default)]
    plugins: Vec<String>,
    #[serde(default)]
    apps: Vec<String>,
    #[serde(flatten)]
    extensions: IndexMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LoopWire {
    max_turns: u32,
    max_tool_rounds: u32,
    #[serde(flatten)]
    extensions: IndexMap<String, Value>,
}

fn default_personality_preset() -> PersonalityPreset {
    PersonalityPreset::Helpful
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersonalityWire {
    #[serde(default = "default_personality_preset")]
    preset: PersonalityPreset,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    custom: Option<String>,
    #[serde(flatten)]
    extensions: IndexMap<String, Value>,
}

impl Default for PersonalityWire {
    fn default() -> Self {
        Self {
            preset: default_personality_preset(),
            custom: None,
            extensions: IndexMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AgentProfileWire {
    schema_version: u32,
    id: String,
    name: String,
    description: String,
    avatar: AvatarWire,
    model: AgentModelWire,
    #[serde(default)]
    personality: PersonalityWire,
    permissions: PermissionsWire,
    skills: SkillsWire,
    tools: ToolsWire,
    #[serde(rename = "loop")]
    loop_settings: LoopWire,
    #[serde(flatten)]
    extensions: IndexMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SubagentConfigWire {
    schema_version: u32,
    model: AgentModelWire,
    #[serde(flatten)]
    extensions: IndexMap<String, Value>,
}

#[derive(Debug, Clone)]
pub(crate) struct AgentProfileDocument {
    typed: AgentProfile,
    raw: Value,
    extensions: IndexMap<String, Value>,
}

impl AgentProfileDocument {
    pub(crate) fn typed(&self) -> &AgentProfile {
        &self.typed
    }

    pub(crate) fn extensions(&self) -> &IndexMap<String, Value> {
        &self.extensions
    }

    pub(crate) fn merge_typed(&mut self, profile: AgentProfile) {
        self.typed = profile;
    }
}

#[derive(Debug, Clone)]
pub(crate) struct AgentIndexDocument {
    typed: AgentIndex,
    raw: Value,
    extensions: IndexMap<String, Value>,
}

impl AgentIndexDocument {
    pub(crate) fn typed(&self) -> &AgentIndex {
        &self.typed
    }

    pub(crate) fn extensions(&self) -> &IndexMap<String, Value> {
        &self.extensions
    }

    pub(crate) fn merge_typed(&mut self, value: AgentIndex) {
        self.typed = value;
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SubagentConfigDocument {
    typed: SubagentConfig,
    raw: Value,
    extensions: IndexMap<String, Value>,
}

impl SubagentConfigDocument {
    pub(crate) fn typed(&self) -> &SubagentConfig {
        &self.typed
    }

    pub(crate) fn extensions(&self) -> &IndexMap<String, Value> {
        &self.extensions
    }

    pub(crate) fn merge_typed(&mut self, value: SubagentConfig) {
        self.typed = value;
    }
}

pub fn parse_agent_index(raw: &str) -> anyhow::Result<AgentIndex> {
    Ok(parse_agent_index_document(raw)?.typed)
}

pub fn render_agent_index(value: &AgentIndex) -> anyhow::Result<String> {
    render_yaml(&index_to_wire(value))
}

pub fn parse_subagent_config(raw: &str) -> anyhow::Result<SubagentConfig> {
    Ok(parse_subagent_config_document(raw)?.typed)
}

pub fn render_subagent_config(value: &SubagentConfig) -> anyhow::Result<String> {
    render_yaml(&SubagentConfigWire {
        schema_version: value.schema_version,
        model: model_to_wire(&value.model, IndexMap::new()),
        extensions: IndexMap::new(),
    })
}

pub fn parse_agent_profile(raw: &str) -> anyhow::Result<AgentProfile> {
    Ok(parse_agent_profile_document(raw)?.typed().clone())
}

pub fn render_agent_profile(value: &AgentProfile) -> anyhow::Result<String> {
    render_yaml(&profile_to_wire(value, &IndexMap::new()))
}

pub(crate) fn parse_agent_profile_document(raw: &str) -> anyhow::Result<AgentProfileDocument> {
    let raw_value: Value = serde_yaml::from_str(raw).context("invalid agent profile YAML")?;
    validate_model_union(&raw_value)?;
    let wire: AgentProfileWire = serde_yaml::from_value(raw_value.clone())?;
    ensure_schema(wire.schema_version)?;
    let (typed, extensions) = profile_from_wire(wire)?;
    Ok(AgentProfileDocument {
        typed,
        raw: raw_value,
        extensions,
    })
}

pub(crate) fn render_agent_profile_document(
    value: &AgentProfileDocument,
) -> anyhow::Result<String> {
    let wire = profile_to_wire(&value.typed, &value.extensions);
    merge_and_render(&value.raw, &wire)
}

pub(crate) fn parse_agent_index_document(raw: &str) -> anyhow::Result<AgentIndexDocument> {
    let raw_value: Value = serde_yaml::from_str(raw).context("invalid agent index YAML")?;
    let wire: AgentIndexWire = serde_yaml::from_value(raw_value.clone())?;
    ensure_schema(wire.schema_version)?;
    let typed = index_from_wire(wire)?;
    Ok(AgentIndexDocument {
        extensions: typed.extensions.clone(),
        typed,
        raw: raw_value,
    })
}

pub(crate) fn render_agent_index_document(value: &AgentIndexDocument) -> anyhow::Result<String> {
    merge_and_render(&value.raw, &index_to_wire(&value.typed))
}

pub(crate) fn parse_subagent_config_document(raw: &str) -> anyhow::Result<SubagentConfigDocument> {
    let raw_value: Value = serde_yaml::from_str(raw).context("invalid subagent YAML")?;
    validate_model_union(&raw_value)?;
    let wire: SubagentConfigWire = serde_yaml::from_value(raw_value.clone())?;
    ensure_schema(wire.schema_version)?;
    let (model, model_extensions) = model_from_wire(wire.model)?;
    let mut extensions = wire.extensions;
    if !model_extensions.is_empty() {
        extensions.insert(
            "model".into(),
            Value::Mapping(map_from_index(model_extensions)),
        );
    }
    Ok(SubagentConfigDocument {
        typed: SubagentConfig {
            schema_version: AGENT_SCHEMA_VERSION,
            model,
        },
        raw: raw_value,
        extensions,
    })
}

pub(crate) fn render_subagent_config_document(
    value: &SubagentConfigDocument,
) -> anyhow::Result<String> {
    let model_extensions = nested_extensions(&value.extensions, "model");
    let wire = SubagentConfigWire {
        schema_version: value.typed.schema_version,
        model: model_to_wire(&value.typed.model, model_extensions),
        extensions: top_extensions(&value.extensions, &["model"]),
    };
    merge_and_render(&value.raw, &wire)
}

fn profile_from_wire(
    wire: AgentProfileWire,
) -> anyhow::Result<(AgentProfile, IndexMap<String, Value>)> {
    let (model, model_extensions) = model_from_wire(wire.model)?;
    let mut extensions = wire.extensions;
    add_nested(&mut extensions, "avatar", wire.avatar.extensions);
    add_nested(&mut extensions, "model", model_extensions);
    add_nested(&mut extensions, "personality", wire.personality.extensions);
    add_nested(&mut extensions, "permissions", wire.permissions.extensions);
    add_nested(&mut extensions, "skills", wire.skills.extensions);
    add_nested(&mut extensions, "tools", wire.tools.extensions);
    add_nested(&mut extensions, "loop", wire.loop_settings.extensions);

    let personality = AgentPersonality {
        preset: wire.personality.preset,
        custom: trim_option(wire.personality.custom),
    };
    personality.validate()?;

    let profile = AgentProfile {
        schema_version: AGENT_SCHEMA_VERSION,
        id: required(wire.id, "id")?,
        name: required(wire.name, "name")?,
        description: required(wire.description, "description")?,
        avatar: AgentAvatar {
            color: required(wire.avatar.color, "avatar.color")?,
        },
        model,
        personality,
        permissions: AgentPermissions {
            mode: wire.permissions.mode.runtime_mode(),
            rules: wire
                .permissions
                .rules
                .into_iter()
                .map(|rule| PermissionRule {
                    id: rule.id.trim().to_owned(),
                    tool: rule.tool.trim().to_owned(),
                    decision: rule.decision,
                    command_prefix: trim_option(rule.command_prefix),
                })
                .collect(),
        },
        skills: trim_vec(wire.skills.enabled),
        tools: AgentTools {
            native: trim_vec(wire.tools.native),
            plugins: trim_vec(wire.tools.plugins),
            apps: trim_vec(wire.tools.apps),
        },
        loop_settings: AgentLoop {
            max_turns: wire.loop_settings.max_turns,
            max_tool_rounds: wire.loop_settings.max_tool_rounds,
        },
    };
    Ok((profile, extensions))
}

fn profile_to_wire(value: &AgentProfile, extensions: &IndexMap<String, Value>) -> AgentProfileWire {
    AgentProfileWire {
        schema_version: value.schema_version,
        id: value.id.clone(),
        name: value.name.clone(),
        description: value.description.clone(),
        avatar: AvatarWire {
            color: value.avatar.color.clone(),
            extensions: nested_extensions(extensions, "avatar"),
        },
        model: model_to_wire(&value.model, nested_extensions(extensions, "model")),
        personality: PersonalityWire {
            preset: value.personality.preset,
            custom: value.personality.custom.clone(),
            extensions: nested_extensions(extensions, "personality"),
        },
        permissions: PermissionsWire {
            mode: AgentPermissionMode::from_runtime(value.permissions.mode),
            rules: value
                .permissions
                .rules
                .iter()
                .map(|rule| PermissionRuleWire {
                    id: rule.id.clone(),
                    tool: rule.tool.clone(),
                    decision: rule.decision,
                    command_prefix: rule.command_prefix.clone(),
                })
                .collect(),
            extensions: nested_extensions(extensions, "permissions"),
        },
        skills: SkillsWire {
            enabled: value.skills.clone(),
            extensions: nested_extensions(extensions, "skills"),
        },
        tools: ToolsWire {
            native: value.tools.native.clone(),
            plugins: value.tools.plugins.clone(),
            apps: value.tools.apps.clone(),
            extensions: nested_extensions(extensions, "tools"),
        },
        loop_settings: LoopWire {
            max_turns: value.loop_settings.max_turns,
            max_tool_rounds: value.loop_settings.max_tool_rounds,
            extensions: nested_extensions(extensions, "loop"),
        },
        extensions: top_extensions(
            extensions,
            &[
                "avatar",
                "model",
                "personality",
                "permissions",
                "skills",
                "tools",
                "loop",
            ],
        ),
    }
}

fn model_from_wire(wire: AgentModelWire) -> anyhow::Result<(AgentModel, IndexMap<String, Value>)> {
    match wire {
        AgentModelWire::Concrete(wire) => Ok((
            AgentModel::Concrete {
                name: required(wire.name, "model.name")?,
                effort: trim_option(wire.effort),
            },
            wire.extensions,
        )),
        AgentModelWire::Route(wire) => Ok((
            AgentModel::Route {
                route: required(wire.route, "model.route")?,
            },
            wire.extensions,
        )),
    }
}

fn model_to_wire(value: &AgentModel, extensions: IndexMap<String, Value>) -> AgentModelWire {
    match value {
        AgentModel::Concrete { name, effort } => AgentModelWire::Concrete(ConcreteModelWire {
            name: name.clone(),
            effort: effort.clone(),
            extensions,
        }),
        AgentModel::Route { route } => AgentModelWire::Route(RouteModelWire {
            route: route.clone(),
            extensions,
        }),
    }
}

fn validate_model_union(value: &Value) -> anyhow::Result<()> {
    let model = value
        .as_mapping()
        .and_then(|map| map.get(Value::String("model".into())))
        .and_then(Value::as_mapping)
        .context("agent model must be a mapping")?;
    let has_name = model.contains_key(Value::String("name".into()));
    let has_route = model.contains_key(Value::String("route".into()));
    if has_name == has_route {
        bail!("agent model requires exactly one of 'name' or 'route'");
    }
    if has_route && model.contains_key(Value::String("effort".into())) {
        bail!("agent route model cannot contain 'effort'");
    }
    Ok(())
}

fn index_from_wire(wire: AgentIndexWire) -> anyhow::Result<AgentIndex> {
    Ok(AgentIndex {
        schema_version: AGENT_SCHEMA_VERSION,
        order: trim_vec(wire.order),
        default_agent_id: required(wire.default_agent_id, "default_agent_id")?,
        extensions: wire.extensions,
    })
}

fn index_to_wire(value: &AgentIndex) -> AgentIndexWire {
    AgentIndexWire {
        schema_version: value.schema_version,
        order: value.order.clone(),
        default_agent_id: value.default_agent_id.clone(),
        extensions: value.extensions.clone(),
    }
}

fn ensure_schema(version: u32) -> anyhow::Result<()> {
    if version != AGENT_SCHEMA_VERSION {
        bail!("unsupported agent schema version {version}");
    }
    Ok(())
}

fn required(value: String, field: &str) -> anyhow::Result<String> {
    let value = value.trim().to_owned();
    if value.is_empty() {
        bail!("{field} cannot be empty");
    }
    Ok(value)
}

fn trim_option(value: Option<String>) -> Option<String> {
    value.map(|value| value.trim().to_owned())
}

fn trim_vec(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .map(|value| value.trim().to_owned())
        .collect()
}

fn add_nested(target: &mut IndexMap<String, Value>, key: &str, values: IndexMap<String, Value>) {
    if !values.is_empty() {
        target.insert(key.into(), Value::Mapping(map_from_index(values)));
    }
}

fn nested_extensions(values: &IndexMap<String, Value>, key: &str) -> IndexMap<String, Value> {
    values
        .get(key)
        .and_then(Value::as_mapping)
        .map(index_from_map)
        .unwrap_or_default()
}

fn top_extensions(values: &IndexMap<String, Value>, nested: &[&str]) -> IndexMap<String, Value> {
    values
        .iter()
        .filter(|(key, _)| !nested.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn map_from_index(values: IndexMap<String, Value>) -> Mapping {
    values
        .into_iter()
        .map(|(key, value)| (Value::String(key), value))
        .collect()
}

fn index_from_map(values: &Mapping) -> IndexMap<String, Value> {
    values
        .iter()
        .filter_map(|(key, value)| Some((key.as_str()?.to_owned(), value.clone())))
        .collect()
}

fn merge_and_render<T: Serialize>(raw: &Value, typed: &T) -> anyhow::Result<String> {
    let mut merged = raw.clone();
    let replacement = serde_yaml::to_value(typed)?;
    remove_stale_model_keys(&mut merged, &replacement);
    remove_stale_personality_keys(&mut merged, &replacement);
    merge_value(&mut merged, replacement);
    render_yaml(&merged)
}

fn remove_stale_model_keys(target: &mut Value, replacement: &Value) {
    let model_key = Value::String("model".into());
    let Some(replacement_model) = replacement
        .as_mapping()
        .and_then(|mapping| mapping.get(&model_key))
        .and_then(Value::as_mapping)
    else {
        return;
    };
    let Some(target_model) = target
        .as_mapping_mut()
        .and_then(|mapping| mapping.get_mut(&model_key))
        .and_then(Value::as_mapping_mut)
    else {
        return;
    };

    for key in ["name", "route", "effort"] {
        let key = Value::String(key.into());
        if !replacement_model.contains_key(&key) {
            target_model.remove(&key);
        }
    }
}

fn remove_stale_personality_keys(target: &mut Value, replacement: &Value) {
    let personality_key = Value::String("personality".into());
    let Some(replacement_personality) = replacement
        .as_mapping()
        .and_then(|mapping| mapping.get(&personality_key))
        .and_then(Value::as_mapping)
    else {
        return;
    };
    let Some(target_personality) = target
        .as_mapping_mut()
        .and_then(|mapping| mapping.get_mut(&personality_key))
        .and_then(Value::as_mapping_mut)
    else {
        return;
    };

    let custom_key = Value::String("custom".into());
    if !replacement_personality.contains_key(&custom_key) {
        target_personality.remove(&custom_key);
    }
}

fn merge_value(target: &mut Value, replacement: Value) {
    match (target, replacement) {
        (Value::Mapping(target), Value::Mapping(replacement)) => {
            for (key, value) in replacement {
                match target.get_mut(&key) {
                    Some(existing) => merge_value(existing, value),
                    None => {
                        target.insert(key, value);
                    }
                }
            }
        }
        (target, replacement) => *target = replacement,
    }
}

fn render_yaml<T: Serialize>(value: &T) -> anyhow::Result<String> {
    let rendered = serde_yaml::to_string(value)?;
    Ok(format!("{}\n", rendered.trim_end()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_roundtrip_preserves_unknown_fields_and_model_union() {
        let raw = r#"schema_version: 1
id: reviewer
name: Reviewer
description: Reviews code.
avatar: { color: violet, x_icon: owl }
model: { name: anthropic/claude-opus-4-8, effort: high, x_model: keep }
permissions: { mode: ask, rules: [], x_policy: keep }
skills: { enabled: [systematic-debugging] }
tools: { native: [read], plugins: [], apps: [] }
loop: { max_turns: 50, max_tool_rounds: 100 }
x_vendor: { enabled: true }
"#;
        let profile = parse_agent_profile_document(raw).unwrap();
        assert!(matches!(profile.typed().model, AgentModel::Concrete { .. }));
        let reparsed =
            parse_agent_profile_document(&render_agent_profile_document(&profile).unwrap())
                .unwrap();
        assert_eq!(reparsed.extensions()["x_vendor"]["enabled"], true);
        assert_eq!(reparsed.extensions()["avatar"]["x_icon"], "owl");
        assert_eq!(reparsed.extensions()["model"]["x_model"], "keep");
        assert_eq!(reparsed.extensions()["permissions"]["x_policy"], "keep");
    }

    const LEGACY_AGENT_YAML: &str = r#"schema_version: 1
id: reviewer
name: Reviewer
description: Reviews code.
avatar: { color: violet }
model: { name: anthropic/claude-opus-4-8, effort: high }
permissions: { mode: ask, rules: [] }
skills: { enabled: [] }
tools: { native: [], plugins: [], apps: [] }
loop: { max_turns: 50, max_tool_rounds: 100 }
"#;

    const CUSTOM_PERSONALITY_YAML: &str = r#"schema_version: 1
id: reviewer
name: Reviewer
description: Reviews code.
avatar: { color: violet }
model: { name: anthropic/claude-opus-4-8, effort: high }
personality: { preset: custom, custom: "Speak like a strict code reviewer.\nBe terse and cite line numbers.", x-user-extension: keep }
permissions: { mode: ask, rules: [] }
skills: { enabled: [] }
tools: { native: [], plugins: [], apps: [] }
loop: { max_turns: 50, max_tool_rounds: 100 }
"#;

    #[test]
    fn missing_legacy_personality_defaults_to_helpful() {
        let doc = parse_agent_profile_document(LEGACY_AGENT_YAML).unwrap();
        assert_eq!(doc.typed().personality.preset, PersonalityPreset::Helpful);
        assert_eq!(doc.typed().personality.custom, None);
    }

    #[test]
    fn custom_personality_round_trips_and_extensions_survive() {
        let parsed = parse_agent_profile_document(CUSTOM_PERSONALITY_YAML).unwrap();
        assert_eq!(parsed.typed().personality.preset, PersonalityPreset::Custom);
        assert_eq!(
            parsed.typed().personality.custom.as_deref(),
            Some("Speak like a strict code reviewer.\nBe terse and cite line numbers.")
        );
        let rendered = render_agent_profile_document(&parsed).unwrap();
        assert!(rendered.contains("preset: custom"));
        assert!(rendered.contains("custom: |-"));
        assert!(rendered.contains("x-user-extension:"));

        let reparsed = parse_agent_profile_document(&rendered).unwrap();
        assert_eq!(reparsed.typed().personality, parsed.typed().personality);
        assert_eq!(
            reparsed.extensions()["personality"]["x-user-extension"],
            "keep"
        );
    }

    #[test]
    fn invalid_personality_combination_is_rejected() {
        let raw = r#"schema_version: 1
id: reviewer
name: Reviewer
description: Reviews code.
avatar: { color: violet }
model: { name: anthropic/claude-opus-4-8, effort: high }
personality: { preset: custom }
permissions: { mode: ask, rules: [] }
skills: { enabled: [] }
tools: { native: [], plugins: [], apps: [] }
loop: { max_turns: 50, max_tool_rounds: 100 }
"#;
        assert!(parse_agent_profile_document(raw).is_err());

        let raw = r#"schema_version: 1
id: reviewer
name: Reviewer
description: Reviews code.
avatar: { color: violet }
model: { name: anthropic/claude-opus-4-8, effort: high }
personality: { preset: technical, custom: "extra text" }
permissions: { mode: ask, rules: [] }
skills: { enabled: [] }
tools: { native: [], plugins: [], apps: [] }
loop: { max_turns: 50, max_tool_rounds: 100 }
"#;
        assert!(parse_agent_profile_document(raw).is_err());
    }

    #[test]
    fn route_model_rejects_effort_and_both_union_arms() {
        for raw in [
            "schema_version: 1\nmodel: { route: free, effort: high }\n",
            "schema_version: 1\nmodel: { route: free, name: openai/gpt-5 }\n",
        ] {
            assert!(parse_subagent_config(raw).is_err(), "accepted {raw}");
        }
    }

    #[test]
    fn merge_typed_model_arm_switch_renders_reparseable_yaml() {
        let raw = r#"schema_version: 1
id: reviewer
name: Reviewer
description: Reviews code.
avatar: { color: violet }
model: { name: anthropic/claude-opus-4-8, effort: high, x_model: keep }
permissions: { mode: ask, rules: [] }
skills: { enabled: [] }
tools: { native: [], plugins: [], apps: [] }
loop: { max_turns: 50, max_tool_rounds: 100 }
"#;
        let mut doc = parse_agent_profile_document(raw).unwrap();
        let mut typed = doc.typed().clone();
        typed.model = AgentModel::Route {
            route: "free".into(),
        };
        doc.merge_typed(typed);
        let rendered = render_agent_profile_document(&doc).unwrap();
        let reparsed = parse_agent_profile_document(&rendered).unwrap();
        assert_eq!(
            reparsed.typed().model,
            AgentModel::Route {
                route: "free".into()
            }
        );
        assert_eq!(reparsed.extensions()["model"]["x_model"], "keep");

        // Dropping an explicit effort inside the concrete arm must also
        // remove the stale `effort` key from the rendered YAML.
        let mut doc = parse_agent_profile_document(raw).unwrap();
        let mut typed = doc.typed().clone();
        typed.model = AgentModel::Concrete {
            name: "anthropic/claude-opus-4-8".into(),
            effort: None,
        };
        doc.merge_typed(typed);
        let rendered = render_agent_profile_document(&doc).unwrap();
        assert!(!rendered.contains("effort"), "stale effort in:\n{rendered}");
        let reparsed = parse_agent_profile_document(&rendered).unwrap();
        assert_eq!(
            reparsed.typed().model,
            AgentModel::Concrete {
                name: "anthropic/claude-opus-4-8".into(),
                effort: None,
            }
        );
    }

    #[test]
    fn merge_typed_personality_preset_switch_renders_reparseable_yaml() {
        let mut doc = parse_agent_profile_document(CUSTOM_PERSONALITY_YAML).unwrap();
        let mut typed = doc.typed().clone();
        typed.personality = AgentPersonality {
            preset: PersonalityPreset::Technical,
            custom: None,
        };
        doc.merge_typed(typed);
        let rendered = render_agent_profile_document(&doc).unwrap();
        assert!(
            !rendered.contains("custom:"),
            "stale custom key in:\n{rendered}"
        );

        let reparsed = parse_agent_profile_document(&rendered).unwrap();
        assert_eq!(
            reparsed.typed().personality,
            AgentPersonality {
                preset: PersonalityPreset::Technical,
                custom: None,
            }
        );
    }

    #[test]
    fn merge_typed_subagent_route_to_concrete_renders_reparseable_yaml() {
        let mut doc =
            parse_subagent_config_document("schema_version: 1\nmodel: { route: free }\n").unwrap();
        let mut typed = doc.typed().clone();
        typed.model = AgentModel::Concrete {
            name: "anthropic/claude-opus-4-8".into(),
            effort: Some("high".into()),
        };
        doc.merge_typed(typed);
        let rendered = render_subagent_config_document(&doc).unwrap();
        let reparsed = parse_subagent_config(&rendered).unwrap();
        assert_eq!(
            reparsed.model,
            AgentModel::Concrete {
                name: "anthropic/claude-opus-4-8".into(),
                effort: Some("high".into()),
            }
        );
    }

    #[test]
    fn index_roundtrip_keeps_order_default_and_extensions() {
        let raw = "schema_version: 1\norder: [b, a]\ndefault_agent_id: b\nx_sync: manual\n";
        let index = parse_agent_index(raw).unwrap();
        assert_eq!(index.order, vec!["b", "a"]);
        assert_eq!(index.default_agent_id, "b");
        assert_eq!(
            parse_agent_index(&render_agent_index(&index).unwrap())
                .unwrap()
                .extensions["x_sync"],
            "manual"
        );
    }
}
