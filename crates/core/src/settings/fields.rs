//! ConfigField schema: the global settings fields. Keys, labels, help
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

/// The global settings fields.
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
        help: "How deep sub-agents may spawn further sub-agents (2 permits a child \
               agent to fan out; 1 = flat)",
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
        key: "listen_addr",
        label: "Listen address",
        default: Some("127.0.0.1"),
        help: "IP the control server binds to (127.0.0.1 = local only; 0.0.0.0 = all interfaces / LAN-reachable). Non-loopback requires pairing over TLS.",
        example: Some("127.0.0.1"),
        ..BASE
    },
    ConfigField {
        key: "context.auto_compact_percent",
        label: "Auto-compact threshold (%)",
        field_type: FieldType::Int,
        default: Some("90"),
        help: "Compact a native session when its context reaches this percent of the model window",
        ..BASE
    },
    ConfigField {
        key: "context.tool_output_max_bytes",
        label: "Tool output budget (bytes)",
        field_type: FieldType::Int,
        default: Some("10000"),
        help: "Per-tool-result byte budget kept in model context (middle-truncated beyond this)",
        ..BASE
    },
    ConfigField {
        key: "context.max_output_tokens",
        label: "Max output tokens",
        field_type: FieldType::Int,
        default: Some("0"),
        help: "Cap on max_tokens per request; 0 = use the model's own maximum",
        ..BASE
    },
    ConfigField {
        key: "context.compact_prompt",
        label: "Compaction prompt",
        help: "Custom summarization prompt for context compaction (blank = built-in)",
        ..BASE
    },
    ConfigField {
        key: "native_tools.version",
        label: "Native tools contract",
        field_type: FieldType::Enum,
        one_of: &["v1", "v2"],
        default: Some("v1"),
        control: true,
        help: "Native tool facade for newly started native sessions",
        ..BASE
    },
    ConfigField {
        key: "artifact_root",
        label: "Artifact storage root",
        help: "Directory task artifact payloads are stored under (blank = default data-dir/artifacts)",
        example: Some("/data/ryuzi/artifacts"),
        ..BASE
    },
    ConfigField {
        key: "artifact_max_bytes",
        label: "Artifact max bytes",
        field_type: FieldType::Int,
        default: Some("26214400"),
        help: "Max size per stored artifact, in bytes (default 25 MiB)",
        ..BASE
    },
    ConfigField {
        key: "artifact_session_max_bytes",
        label: "Artifact session quota (bytes)",
        field_type: FieldType::Int,
        default: Some("262144000"),
        help: "Max total bytes of artifacts a single source session may persist (default 250 MiB)",
        ..BASE
    },
    ConfigField {
        key: "artifact_read_max_bytes",
        label: "Artifact read cap (bytes)",
        field_type: FieldType::Int,
        default: Some("50000"),
        help: "Max bytes returned by a single artifact read (larger reads are truncated)",
        ..BASE
    },
    ConfigField {
        key: "artifact_retention_days",
        label: "Artifact retention (days)",
        field_type: FieldType::Int,
        default: Some("30"),
        help: "Days a deleted-source artifact's payload is kept before permanent removal",
        ..BASE
    },
];

#[cfg(test)]
mod tests {
    use crate::settings::{all_fields, find_field};

    #[test]
    fn schema_has_35_keys_and_correct_flags() {
        let fields = all_fields();
        assert_eq!(fields.len(), 35); // 32 global + 3 discord
        let keys: Vec<&str> = fields.iter().map(|f| f.key).collect();
        // list order: 32 globals first, then 3 discord fields
        assert_eq!(keys[0], "workdir_root");
        assert!(keys.contains(&"max_spawn_depth"));
        assert!(keys.contains(&"approval_timeout_ms"));
        assert_eq!(
            &keys[32..],
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
        assert_eq!(
            find_field("context.auto_compact_percent").unwrap().default,
            Some("90")
        );
        assert_eq!(
            find_field("context.tool_output_max_bytes").unwrap().default,
            Some("10000")
        );
        assert_eq!(find_field("artifact_root").unwrap().default, None);
        assert_eq!(
            find_field("artifact_max_bytes").unwrap().default,
            Some("26214400")
        );
        assert_eq!(
            find_field("artifact_session_max_bytes").unwrap().default,
            Some("262144000")
        );
        assert_eq!(
            find_field("artifact_read_max_bytes").unwrap().default,
            Some("50000")
        );
        assert_eq!(
            find_field("artifact_retention_days").unwrap().default,
            Some("30")
        );
    }

    #[test]
    fn native_tools_version_is_a_validated_rollout_setting() {
        let field = crate::settings::find_field("native_tools.version").unwrap();
        assert_eq!(field.default, Some("v1"));
        assert_eq!(field.one_of, &["v1", "v2"]);
        assert!(crate::settings::validate_setting("native_tools.version", "v2").is_none());
        assert!(crate::settings::validate_setting("native_tools.version", "terra").is_some());
    }
}
