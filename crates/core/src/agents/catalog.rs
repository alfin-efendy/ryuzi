//! Catalog of configuration options (skills, native tools, plugin tools, apps)
//! available for agent profiles, and validation of profile references against it.

use super::types::{AgentMutationInput, AgentProfile, AgentValidationIssue, PermissionRule};
use crate::harness::native::tools::ToolRegistry;
use crate::mcp;
use crate::plugins::PluginHost;
use crate::settings::SettingsStore;
use crate::skills_install::list_installed_skills;
use crate::store::Store;

/// Native tool ids whose permission rules can carry a `command_prefix`
/// (i.e. are "command scoped"). Kept as a single list so the catalog
/// builder and [`native_tool_is_command_scoped`] cannot drift apart.
pub const COMMAND_SCOPED_NATIVE_TOOLS: &[&str] = &["bash"];

/// Whether `id` is a native tool id in [`COMMAND_SCOPED_NATIVE_TOOLS`].
pub fn native_tool_is_command_scoped(id: &str) -> bool {
    COMMAND_SCOPED_NATIVE_TOOLS.contains(&id)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    pub id: String,
    pub label: String,
    pub description: String,
    pub available: bool,
    pub command_scoped: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentConfigurationCatalog {
    pub skills: Vec<CatalogEntry>,
    pub native_tools: Vec<CatalogEntry>,
    pub plugin_tools: Vec<CatalogEntry>,
    pub apps: Vec<CatalogEntry>,
}

impl AgentConfigurationCatalog {
    pub fn from_entries(entries: impl IntoIterator<Item = CatalogEntry>) -> Self {
        Self {
            skills: Vec::new(),
            native_tools: entries.into_iter().collect(),
            plugin_tools: Vec::new(),
            apps: Vec::new(),
        }
    }

    pub fn contains_native(&self, id: &str) -> bool {
        self.native_tools
            .iter()
            .any(|entry| entry.id == id && entry.available)
    }

    pub fn contains_skill(&self, id: &str) -> bool {
        self.skills
            .iter()
            .any(|entry| entry.id == id && entry.available)
    }

    pub fn contains_plugin(&self, id: &str) -> bool {
        self.plugin_tools.iter().any(|entry| {
            entry.available
                && (id == entry.id || id.strip_prefix(&format!("{}.", entry.id)).is_some())
        })
    }

    pub fn contains_app(&self, id: &str) -> bool {
        self.apps
            .iter()
            .any(|entry| entry.id == id && entry.available)
    }

    pub fn command_scoped(&self, id: &str) -> bool {
        self.all_entries()
            .find(|entry| entry.id == id)
            .is_some_and(|entry| entry.command_scoped)
    }

    fn all_entries(&self) -> impl Iterator<Item = &CatalogEntry> {
        self.skills
            .iter()
            .chain(self.native_tools.iter())
            .chain(self.plugin_tools.iter())
            .chain(self.apps.iter())
    }

    pub fn contains(&self, id: &str) -> bool {
        self.all_entries()
            .any(|entry| entry.id == id && entry.available)
    }

    pub fn validate_mutation_references(
        input: &AgentMutationInput,
        catalog: &Self,
    ) -> Vec<AgentValidationIssue> {
        validate_references(
            &input.skills,
            &input.tools.native,
            &input.tools.plugins,
            &input.tools.apps,
            &input.permissions.rules,
            catalog,
        )
    }

    pub fn validate_profile_references(
        profile: &AgentProfile,
        catalog: &Self,
    ) -> Vec<AgentValidationIssue> {
        validate_references(
            &profile.skills,
            &profile.tools.native,
            &profile.tools.plugins,
            &profile.tools.apps,
            &profile.permissions.rules,
            catalog,
        )
    }
}

pub async fn build_live_catalog(
    store: &Store,
    plugins: &PluginHost,
) -> anyhow::Result<AgentConfigurationCatalog> {
    let native_tools = ToolRegistry::builtin()
        .iter()
        .map(|(id, tool)| CatalogEntry {
            id: id.to_string(),
            label: tool.tool.name().to_string(),
            description: tool.tool.description().to_string(),
            available: true,
            command_scoped: native_tool_is_command_scoped(id),
        })
        .collect::<Vec<_>>();

    let skills = list_installed_skills()?
        .into_iter()
        .map(|skill| native(&skill.id, &skill.name))
        .collect::<Vec<_>>();

    let settings = SettingsStore::new(std::sync::Arc::new(store.clone()));
    let mut plugin_tools = Vec::new();
    for plugin in plugins.list() {
        if !plugins.is_enabled(&settings, &plugin.manifest.id).await? {
            continue;
        }
        plugin_tools.push(CatalogEntry {
            id: plugin.manifest.id.clone(),
            label: plugin.manifest.name.clone(),
            description: plugin.manifest.description.clone(),
            available: true,
            command_scoped: false,
        });
    }

    let apps = mcp::list_servers(store)
        .await?
        .into_iter()
        .map(|row| CatalogEntry {
            id: row.id,
            label: row.name,
            description: row.description,
            available: true,
            command_scoped: false,
        })
        .collect::<Vec<_>>();

    Ok(AgentConfigurationCatalog {
        skills,
        native_tools,
        plugin_tools,
        apps,
    })
}

pub fn runtime_profile_executable(
    profile: &AgentProfile,
    structural_executable: bool,
    catalog: &AgentConfigurationCatalog,
) -> bool {
    structural_executable
        && AgentConfigurationCatalog::validate_profile_references(profile, catalog).is_empty()
}

fn validate_references(
    skills: &[String],
    native_tools: &[String],
    plugin_tools: &[String],
    apps: &[String],
    rules: &[PermissionRule],
    catalog: &AgentConfigurationCatalog,
) -> Vec<AgentValidationIssue> {
    let mut issues = Vec::new();

    for id in skills {
        if !catalog.contains_skill(id) {
            issues.push(AgentValidationIssue {
                field: "skills".to_string(),
                message: format!("unknown or unavailable skill: {id}"),
            });
        }
    }

    for id in native_tools {
        if !catalog.contains_native(id) {
            issues.push(AgentValidationIssue {
                field: "tools.native".to_string(),
                message: format!("unknown or unavailable native tool: {id}"),
            });
        }
    }

    for id in plugin_tools {
        if !catalog.contains_plugin(id) {
            issues.push(AgentValidationIssue {
                field: "tools.plugins".to_string(),
                message: format!("unknown or unavailable plugin tool: {id}"),
            });
        }
    }

    for id in apps {
        if !catalog.contains_app(id) {
            issues.push(AgentValidationIssue {
                field: "tools.apps".to_string(),
                message: format!("unknown or unavailable app: {id}"),
            });
        }
    }

    let mut seen: Vec<(&str, Option<&str>)> = Vec::new();
    for rule in rules {
        let key = (rule.tool.as_str(), rule.command_prefix.as_deref());
        if seen.contains(&key) {
            issues.push(AgentValidationIssue {
                field: "permissions.rules".to_string(),
                message: format!(
                    "duplicate permission rule for tool {:?} with command prefix {:?}",
                    rule.tool, rule.command_prefix
                ),
            });
        } else {
            seen.push(key);
        }

        if has_nonempty_command_prefix(rule) && !catalog.command_scoped(&rule.tool) {
            issues.push(AgentValidationIssue {
                field: "permissions.rules".to_string(),
                message: format!(
                    "tool {:?} does not support command-scoped permission rules",
                    rule.tool
                ),
            });
        }
    }

    issues
}

fn has_nonempty_command_prefix(rule: &PermissionRule) -> bool {
    rule.command_prefix
        .as_deref()
        .is_some_and(|prefix| !prefix.is_empty())
}

pub fn native(id: &str, label: &str) -> CatalogEntry {
    CatalogEntry {
        id: id.to_string(),
        label: label.to_string(),
        description: String::new(),
        available: true,
        command_scoped: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::personality::AgentPersonality;
    use crate::agents::types::{AgentAvatar, AgentLoop, AgentModel, AgentPermissions, AgentTools};
    use crate::PermMode;

    fn base_profile(native_tools: Vec<String>, rules: Vec<PermissionRule>) -> AgentProfile {
        AgentProfile {
            schema_version: 1,
            id: "test-agent".to_string(),
            name: "Test Agent".to_string(),
            description: String::new(),
            avatar: AgentAvatar {
                color: "blue".to_string(),
            },
            model: AgentModel::Route {
                route: "default".to_string(),
            },
            personality: AgentPersonality::default_profile(),
            permissions: AgentPermissions {
                mode: PermMode::Default,
                rules,
            },
            skills: Vec::new(),
            tools: AgentTools {
                native: native_tools,
                plugins: Vec::new(),
                apps: Vec::new(),
            },
            loop_settings: AgentLoop {
                max_turns: 1,
                max_tool_rounds: 1,
            },
        }
    }

    fn rule(id: &str, tool: &str, command_prefix: Option<&str>) -> PermissionRule {
        PermissionRule {
            id: id.to_string(),
            tool: tool.to_string(),
            decision: crate::agents::types::PermissionDecision::Allow,
            command_prefix: command_prefix.map(|s| s.to_string()),
        }
    }

    #[test]
    fn missing_native_tool_produces_tools_native_issue() {
        let catalog = AgentConfigurationCatalog::from_entries(vec![native("bash", "Bash")]);
        let profile = base_profile(vec!["bash".to_string(), "missing_tool".to_string()], vec![]);

        let issues = AgentConfigurationCatalog::validate_profile_references(&profile, &catalog);

        assert!(issues
            .iter()
            .any(|issue| issue.field == "tools.native" && issue.message.contains("missing_tool")));
    }

    #[test]
    fn mutation_references_reject_unknown_skill_and_native_tool() {
        let catalog = AgentConfigurationCatalog::from_entries(vec![native("bash", "Bash")]);
        let profile = base_profile(vec!["missing_tool".to_string()], vec![]);
        let input = AgentMutationInput {
            name: profile.name,
            description: profile.description,
            avatar: profile.avatar,
            model: profile.model,
            personality: profile.personality,
            permissions: profile.permissions,
            skills: vec!["missing_skill".to_string()],
            tools: profile.tools,
            loop_settings: profile.loop_settings,
        };

        let issues = AgentConfigurationCatalog::validate_mutation_references(&input, &catalog);

        assert!(issues
            .iter()
            .any(|issue| { issue.field == "skills" && issue.message.contains("missing_skill") }));
        assert!(issues.iter().any(|issue| {
            issue.field == "tools.native" && issue.message.contains("missing_tool")
        }));
    }
    #[test]
    fn contains_plugin_accepts_namespaced_tool_and_rejects_missing() {
        let catalog = AgentConfigurationCatalog {
            plugin_tools: vec![native("github", "GitHub")],
            ..AgentConfigurationCatalog::default()
        };

        assert!(catalog.contains_plugin("github.search"));
        assert!(!catalog.contains_plugin("missing.search"));
    }

    #[test]
    fn structural_profile_with_unavailable_reference_is_not_runtime_executable() {
        let profile = base_profile(vec!["missing".to_string()], vec![]);
        assert!(!runtime_profile_executable(
            &profile,
            true,
            &AgentConfigurationCatalog::default()
        ));
    }

    #[test]
    fn duplicate_permission_rule_pair_is_rejected() {
        let catalog = AgentConfigurationCatalog::from_entries(vec![native("bash", "Bash")]);
        let profile = base_profile(
            vec!["bash".to_string()],
            vec![
                rule("r1", "bash", Some("git ")),
                rule("r2", "bash", Some("git ")),
            ],
        );

        let issues = AgentConfigurationCatalog::validate_profile_references(&profile, &catalog);

        assert!(
            issues
                .iter()
                .any(|issue| issue.field == "permissions.rules"
                    && issue.message.contains("duplicate"))
        );
    }
}
