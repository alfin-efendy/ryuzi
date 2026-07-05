//! ConfigField schema: the 22 global settings fields. Keys, labels, help
//! text, and defaults are user-visible contracts — settings stored under
//! these keys must keep resolving across releases.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType {
    String,
    Int,
    Enum,
}

#[derive(Debug, Clone, Copy)]
pub struct ConfigField {
    pub key: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub example: Option<&'static str>,
    pub secret: bool,
    pub required: bool,
    pub control: bool,
    pub field_type: FieldType,
    pub one_of: &'static [&'static str],
    pub default: Option<&'static str>,
}

/// Base `ConfigField` used with struct-update syntax so each entry below
/// only spells out the fields it overrides.
pub const BASE: ConfigField = ConfigField {
    key: "",
    label: "",
    help: "",
    example: None,
    secret: false,
    required: false,
    control: false,
    field_type: FieldType::String,
    one_of: &[],
    default: None,
};

/// The 22 global settings fields.
pub static GLOBAL_FIELDS: &[ConfigField] = &[
    ConfigField {
        key: "workdir_root",
        label: "Workdir root",
        required: true,
        help: "Parent directory where project repos live",
        example: Some("/home/you/repos"),
        ..BASE
    },
    ConfigField {
        key: "default_model",
        label: "Default model",
        help: "Default model for new projects (blank = harness default)",
        ..BASE
    },
    ConfigField {
        key: "default_effort",
        label: "Default effort",
        default: Some("medium"),
        help: "Default reasoning effort for new projects",
        example: Some("medium"),
        ..BASE
    },
    ConfigField {
        key: "default_perm_mode",
        label: "Default permission mode",
        field_type: FieldType::Enum,
        one_of: &["default", "acceptEdits", "bypassPermissions"],
        default: Some("default"),
        help: "Default approval mode for new projects",
        ..BASE
    },
    ConfigField {
        key: "admin_role_ids",
        label: "Admin role IDs",
        help: "Comma-separated role IDs allowed to administer (gateway-specific)",
        ..BASE
    },
    ConfigField {
        key: "approver_role_ids",
        label: "Approver role IDs",
        help: "Comma-separated role IDs allowed to approve tool use",
        ..BASE
    },
    ConfigField {
        key: "otel_endpoint",
        label: "OTel endpoint",
        help: "OpenTelemetry OTLP/HTTP endpoint (blank = console telemetry)",
        ..BASE
    },
    ConfigField {
        key: "max_concurrent_runs",
        label: "Max concurrent runs",
        field_type: FieldType::Int,
        default: Some("3"),
        help: "Max simultaneous sessions",
        ..BASE
    },
    ConfigField {
        key: "max_spawn_depth",
        label: "Max sub-agent spawn depth",
        field_type: FieldType::Int,
        default: Some("2"),
        help: "How deep sub-agents may spawn further sub-agents (2 lets a \
               delegating agent like `orchestrator` fan out; 1 = flat)",
        ..BASE
    },
    ConfigField {
        key: "approval_timeout_ms",
        label: "Approval timeout (ms)",
        field_type: FieldType::Int,
        default: Some("300000"),
        help: "How long to wait for a tool approval",
        ..BASE
    },
    ConfigField {
        key: "attachment_max_bytes",
        label: "Attachment max bytes",
        field_type: FieldType::Int,
        default: Some("26214400"),
        help: "Max size per downloaded Discord attachment, in bytes (default 25 MB)",
        ..BASE
    },
    ConfigField {
        key: "attachment_max_count",
        label: "Attachment max count",
        field_type: FieldType::Int,
        default: Some("10"),
        help: "Max attachments accepted per message; 0 disables attachments",
        ..BASE
    },
    ConfigField {
        key: "attachment_allowed_ext",
        label: "Attachment allowed extensions",
        help: "Comma-separated allowed file extensions (e.g. png,jpg,pdf); blank = all types",
        ..BASE
    },
    ConfigField {
        key: "attachment_allowed_hosts",
        label: "Attachment allowed hosts",
        default: Some("cdn.discordapp.com,media.discordapp.net"),
        help: "Comma-separated hostnames attachments may be downloaded from; blank = no host restriction",
        ..BASE
    },
    ConfigField {
        key: "auto_update",
        label: "Auto-update",
        field_type: FieldType::Enum,
        one_of: &["auto", "notify", "off"],
        default: Some("auto"),
        help: "Daemon update behavior: auto (self-apply on install.sh installs), notify (announce only), off (don't check)",
        ..BASE
    },
    ConfigField {
        key: "auto_update_check_interval_ms",
        label: "Auto-update check interval (ms)",
        field_type: FieldType::Int,
        default: Some("21600000"),
        help: "How often the daemon checks GitHub Releases for a new version (default 6h)",
        ..BASE
    },
    ConfigField {
        key: "auto_update_drain_timeout_ms",
        label: "Auto-update drain timeout (ms)",
        field_type: FieldType::Int,
        default: Some("300000"),
        help: "Max time to wait for in-flight turns before applying an update (default 5m)",
        ..BASE
    },
    ConfigField {
        key: "auto_update_canary_timeout_ms",
        label: "Auto-update canary timeout (ms)",
        field_type: FieldType::Int,
        default: Some("60000"),
        help: "Max time the canary health-check may take before aborting an update (default 1m)",
        ..BASE
    },
    ConfigField {
        key: "auto_update_repo",
        label: "Auto-update repo",
        default: Some("alfin-efendy/ryuzi"),
        help: "blank = harness default",
        ..BASE
    },
    ConfigField {
        key: "last_notified_version",
        label: "Last notified version",
        control: true,
        help: "(internal) last update version already announced",
        ..BASE
    },
    ConfigField {
        key: "enabled_gateways",
        label: "Enabled gateways",
        control: true,
        help: "(managed by the Providers picker)",
        ..BASE
    },
    ConfigField {
        key: "enabled_runtimes",
        label: "Enabled runtimes",
        control: true,
        help: "(managed by the Providers picker)",
        ..BASE
    },
    ConfigField {
        key: "default_runtime",
        label: "Default runtime",
        control: true,
        help: "(managed by the Providers picker)",
        ..BASE
    },
];

#[cfg(test)]
mod tests {
    use crate::settings::{all_fields, find_field};

    #[test]
    fn schema_has_26_keys_and_correct_flags() {
        let fields = all_fields();
        assert_eq!(fields.len(), 26); // 23 global + 3 discord + 0 claude-code
        let keys: Vec<&str> = fields.iter().map(|f| f.key).collect();
        // list order: globals first, then discord fields
        assert_eq!(keys[0], "workdir_root");
        assert!(keys.contains(&"max_spawn_depth"));
        assert_eq!(
            &keys[23..],
            &["discord.token", "discord.app_id", "discord.guild_id"]
        );
        // the only required global is workdir_root; all 3 discord fields required; token is the only secret
        let required: Vec<&str> = fields
            .iter()
            .filter(|f| f.required)
            .map(|f| f.key)
            .collect();
        assert_eq!(
            required,
            vec![
                "workdir_root",
                "discord.token",
                "discord.app_id",
                "discord.guild_id"
            ]
        );
        let secret: Vec<&str> = fields.iter().filter(|f| f.secret).map(|f| f.key).collect();
        assert_eq!(secret, vec!["discord.token"]);
        assert_eq!(
            find_field("default_effort").unwrap().default,
            Some("medium")
        );
        assert_eq!(
            find_field("default_perm_mode").unwrap().one_of,
            &["default", "acceptEdits", "bypassPermissions"]
        );
    }
}
