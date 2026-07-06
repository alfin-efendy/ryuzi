//! Runtimes domain: the catalog of CLI coding agents (runtimes) Cockpit can
//! drive, real binary detection, and the persisted per-agent configuration
//! overlay (enabled/model/perm-mode/flags/tiers) that feeds session start.
//!
//! Identity (names, colors, npm packages, model lists) is code; only user
//! choices and detection snapshots persist.

use crate::domain::PermMode;
use crate::store::Store;
use rusqlite::{params, OptionalExtension};
use std::path::PathBuf;
use std::time::Duration;

/// Static identity of a supported agent CLI.
pub struct RuntimeDescriptor {
    pub id: &'static str,
    pub name: &'static str,
    pub color: &'static str,
    pub initial: &'static str,
    pub connection: &'static str,
    /// Binary name looked up on PATH.
    pub binary: &'static str,
    /// npm package consulted for the latest released version, if any.
    pub npm_package: Option<&'static str>,
    pub models: &'static [&'static str],
    pub default_model: &'static str,
    /// Default per-tier model routing (tier id, label, value, combo).
    pub tiers: &'static [(&'static str, &'static str, Option<&'static str>, bool)],
}

pub const CATALOG: &[RuntimeDescriptor] = &[
    RuntimeDescriptor {
        id: "native",
        name: "Native (ryuzi)",
        color: "#7C5CFF",
        initial: "R",
        connection: "In-process · your model providers",
        // No external binary: the native runtime runs in-process. Marked
        // always-available in `runtimes_cmd::assemble`.
        binary: "ryuzi",
        npm_package: None,
        // Models come from the configured provider connections (Models screen),
        // not a fixed catalog list.
        models: &[],
        default_model: "",
        tiers: &[
            ("default", "Default", None, false),
            ("plan", "Plan", None, false),
            ("fast", "Fast", None, false),
        ],
    },
    RuntimeDescriptor {
        id: "claude",
        name: "Claude Code",
        color: "#D97757",
        initial: "C",
        connection: "Anthropic API",
        binary: "claude",
        npm_package: Some("@anthropic-ai/claude-code"),
        models: &["claude-opus-4-5", "claude-sonnet-4-5", "claude-haiku-4-5"],
        default_model: "claude-opus-4-5",
        tiers: &[
            ("default", "Default", Some("claude-opus-4-5"), false),
            ("plan", "Plan", Some("claude-opus-4-5"), false),
            ("fast", "Fast", Some("claude-haiku-4-5"), false),
        ],
    },
    RuntimeDescriptor {
        id: "codex",
        name: "OpenAI Codex",
        color: "#0FA47F",
        initial: "O",
        connection: "ChatGPT account",
        binary: "codex",
        npm_package: Some("@openai/codex"),
        models: &["gpt-5.2-codex", "gpt-5.2", "o5-mini"],
        default_model: "gpt-5.2-codex",
        tiers: &[
            ("default", "Default", Some("gpt-5.2-codex"), false),
            ("plan", "Plan", Some("gpt-5.2"), false),
            ("fast", "Fast", None, false),
        ],
    },
    RuntimeDescriptor {
        id: "gemini",
        name: "Gemini CLI",
        color: "#4285F4",
        initial: "G",
        connection: "Google Cloud",
        binary: "gemini",
        npm_package: Some("@google/gemini-cli"),
        models: &["gemini-3.0-pro", "gemini-3.0-flash"],
        default_model: "gemini-3.0-pro",
        tiers: &[
            ("default", "Default", Some("gemini-3.0-pro"), false),
            ("plan", "Plan", None, false),
            ("fast", "Fast", Some("gemini-3.0-flash"), false),
        ],
    },
    RuntimeDescriptor {
        id: "ollama",
        name: "Ollama (local)",
        color: "#8B8B8B",
        initial: "L",
        connection: "localhost:11434",
        binary: "ollama",
        npm_package: None,
        models: &[],
        default_model: "",
        tiers: &[
            ("default", "Default", None, false),
            ("plan", "Plan", None, false),
            ("fast", "Fast", None, false),
        ],
    },
    RuntimeDescriptor {
        id: "opencode",
        name: "OpenCode",
        color: "#F5A623",
        initial: "OC",
        connection: "Multi-provider CLI",
        binary: "opencode",
        npm_package: Some("opencode-ai"),
        models: &[],
        default_model: "",
        tiers: &[
            ("default", "Default", None, false),
            ("plan", "Plan", None, false),
            ("fast", "Fast", None, false),
        ],
    },
];

pub fn descriptor(id: &str) -> Option<&'static RuntimeDescriptor> {
    CATALOG.iter().find(|d| d.id == id)
}

/// Persisted per-agent user configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeConfig {
    pub id: String,
    pub enabled: bool,
    pub model: Option<String>,
    /// UI permission mode: plan | ask | edit | full.
    pub perm_mode: String,
    pub flags: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TierRow {
    pub tier_id: String,
    pub label: String,
    pub value: Option<String>,
    pub combo: bool,
}

/// Map the UI agent permission mode onto the engine's session `PermMode`.
pub fn ui_perm_to_core(mode: &str) -> PermMode {
    match mode {
        "plan" => PermMode::Plan,
        "edit" => PermMode::AcceptEdits,
        "full" => PermMode::BypassPermissions,
        _ => PermMode::Default, // "ask" and anything unknown
    }
}

/// Locate `bin` on PATH, honoring PATHEXT on Windows (npm shims install
/// `claude.cmd` etc., not `.exe`).
pub fn find_on_path(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
            .split(';')
            .map(|s| s.to_ascii_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        for ext in &exts {
            let cand = dir.join(format!("{bin}{ext}"));
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// Extract the first `MAJOR.MINOR[.PATCH…]` token from a `--version` output
/// line like `2.1.4 (Claude Code)` or `ollama version is 0.6.4`.
pub fn parse_version(output: &str) -> Option<String> {
    for token in output.split_whitespace() {
        let t = token.trim_matches(|c: char| !(c.is_ascii_digit() || c == '.'));
        let mut parts = t.split('.');
        let looks_semver = t.contains('.')
            && parts.clone().count() >= 2
            && parts.all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
        if looks_semver {
            return Some(t.to_string());
        }
    }
    None
}

/// Live detection result for one agent.
#[derive(Debug, Clone, PartialEq)]
pub struct Detection {
    pub binary_path: Option<String>,
    pub installed_version: Option<String>,
}

/// Probe one agent binary: PATH lookup + `--version` (5s timeout).
pub async fn detect(binary: &str) -> Detection {
    let Some(path) = find_on_path(binary) else {
        return Detection {
            binary_path: None,
            installed_version: None,
        };
    };
    let version = run_version_probe(&path).await;
    Detection {
        binary_path: Some(path.to_string_lossy().into_owned()),
        installed_version: version,
    }
}

async fn run_version_probe(path: &PathBuf) -> Option<String> {
    // .cmd/.bat shims must run through cmd.exe.
    let is_shim = matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("cmd") | Some("bat")
    );
    let mut cmd = if cfg!(windows) && is_shim {
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg(path).arg("--version");
        c
    } else {
        let mut c = tokio::process::Command::new(path);
        c.arg("--version");
        c
    };
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = tokio::time::timeout(Duration::from_secs(5), cmd.output())
        .await
        .ok()?
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    parse_version(&text).or_else(|| parse_version(&String::from_utf8_lossy(&out.stderr)))
}

/// Installed local models reported by `ollama list` (empty when unavailable).
pub async fn ollama_models(path: &str) -> Vec<String> {
    let mut cmd = tokio::process::Command::new(path);
    cmd.arg("list")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let Ok(Ok(out)) = tokio::time::timeout(Duration::from_secs(5), cmd.output()).await else {
        return vec![];
    };
    parse_ollama_list(&String::from_utf8_lossy(&out.stdout))
}

/// First column of `ollama list`, skipping the NAME header row.
pub fn parse_ollama_list(text: &str) -> Vec<String> {
    text.lines()
        .skip(1)
        .filter_map(|l| l.split_whitespace().next())
        .map(|s| s.to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

// NOTE: the backing tables keep their legacy names `agents` / `agent_tiers`
// (and the settings key `default_agent`) — renaming stored identifiers buys
// nothing and would need a data migration.
pub async fn list_configs(store: &Store) -> anyhow::Result<Vec<RuntimeConfig>> {
    store
        .with_conn(|c| {
            let mut stmt = c.prepare("SELECT id, enabled, model, perm_mode, flags FROM agents")?;
            let rows = stmt
                .query_map([], |r| {
                    Ok(RuntimeConfig {
                        id: r.get(0)?,
                        enabled: r.get::<_, i64>(1)? != 0,
                        model: r.get(2)?,
                        perm_mode: r.get(3)?,
                        flags: r.get(4)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
}

pub async fn get_config(store: &Store, id: &str) -> anyhow::Result<Option<RuntimeConfig>> {
    let id = id.to_string();
    store
        .with_conn(move |c| {
            c.query_row(
                "SELECT id, enabled, model, perm_mode, flags FROM agents WHERE id=?1",
                params![id],
                |r| {
                    Ok(RuntimeConfig {
                        id: r.get(0)?,
                        enabled: r.get::<_, i64>(1)? != 0,
                        model: r.get(2)?,
                        perm_mode: r.get(3)?,
                        flags: r.get(4)?,
                    })
                },
            )
            .optional()
        })
        .await
}

pub async fn upsert_config(store: &Store, cfg: RuntimeConfig) -> anyhow::Result<()> {
    store
        .with_conn(move |c| {
            c.execute(
                "INSERT INTO agents(id, enabled, model, perm_mode, flags) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(id) DO UPDATE SET \
                   enabled=excluded.enabled, model=excluded.model, \
                   perm_mode=excluded.perm_mode, flags=excluded.flags",
                params![
                    cfg.id,
                    cfg.enabled as i64,
                    cfg.model,
                    cfg.perm_mode,
                    cfg.flags
                ],
            )
            .map(|_| ())
        })
        .await
}

/// Tiers for `agent_id`: persisted rows merged over the catalog defaults, so
/// the UI always sees the full tier list.
pub async fn list_tiers(store: &Store, agent_id: &str) -> anyhow::Result<Vec<TierRow>> {
    let desc = descriptor(agent_id);
    let id = agent_id.to_string();
    let persisted: Vec<(String, Option<String>, bool)> = store
        .with_conn(move |c| {
            let mut stmt =
                c.prepare("SELECT tier_id, value, combo FROM agent_tiers WHERE agent_id=?1")?;
            let rows = stmt
                .query_map(params![id], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? != 0))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await?;
    let defaults = desc.map(|d| d.tiers).unwrap_or(&[]);
    Ok(defaults
        .iter()
        .map(
            |(tid, label, value, combo)| match persisted.iter().find(|(p, _, _)| p == tid) {
                Some((_, v, cb)) => TierRow {
                    tier_id: tid.to_string(),
                    label: label.to_string(),
                    value: v.clone(),
                    combo: *cb,
                },
                None => TierRow {
                    tier_id: tid.to_string(),
                    label: label.to_string(),
                    value: value.map(|s| s.to_string()),
                    combo: *combo,
                },
            },
        )
        .collect())
}

pub async fn set_tier(
    store: &Store,
    agent_id: &str,
    tier_id: &str,
    value: Option<String>,
    combo: bool,
) -> anyhow::Result<()> {
    let agent_id = agent_id.to_string();
    let tier_id = tier_id.to_string();
    store
        .with_conn(move |c| {
            c.execute(
                "INSERT INTO agent_tiers(agent_id, tier_id, value, combo) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(agent_id, tier_id) DO UPDATE SET \
                   value=excluded.value, combo=excluded.combo",
                params![agent_id, tier_id, value, combo as i64],
            )
            .map(|_| ())
        })
        .await
}

/// Session parameters derived from the default agent's config: used by
/// `ControlPlane::start_session` when the project doesn't pin its own.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionDefaults {
    pub model: Option<String>,
    pub perm_mode: Option<PermMode>,
}

/// Build the npm argv for updating `pkg` (separated for testability).
pub fn npm_update_argv(pkg: &str) -> Vec<String> {
    vec!["install".into(), "-g".into(), format!("{pkg}@latest")]
}

/// Run `npm install -g <pkg>@latest`, streaming each output line as a
/// RuntimeUpdateLog event. Returns Ok(exit_success).
pub async fn run_npm_update(
    events: tokio::sync::broadcast::Sender<crate::domain::CoreEvent>,
    id: &str,
    pkg: &str,
) -> anyhow::Result<bool> {
    use tokio::io::AsyncBufReadExt;
    let args = npm_update_argv(pkg);
    // npm is a .cmd shim on Windows — run through cmd.exe like the version probe.
    let mut cmd = if cfg!(windows) {
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg("npm").args(&args);
        c
    } else {
        let mut c = tokio::process::Command::new("npm");
        c.args(&args);
        c
    };
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn()?;
    let mut out = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
    let mut errl = tokio::io::BufReader::new(child.stderr.take().unwrap()).lines();
    let (id_a, id_b) = (id.to_string(), id.to_string());
    let (ev_a, ev_b) = (events.clone(), events.clone());
    let a = tokio::spawn(async move {
        while let Ok(Some(line)) = out.next_line().await {
            let _ = ev_a.send(crate::domain::CoreEvent::RuntimeUpdateLog {
                runtime_id: id_a.clone(),
                line,
            });
        }
    });
    let b = tokio::spawn(async move {
        while let Ok(Some(line)) = errl.next_line().await {
            let _ = ev_b.send(crate::domain::CoreEvent::RuntimeUpdateLog {
                runtime_id: id_b.clone(),
                line,
            });
        }
    });
    let status = child.wait().await?;
    let _ = a.await;
    let _ = b.await;
    Ok(status.success())
}

/// Map a project's `harness` id (as stored on the project row) to the runtime
/// catalog id whose config the session should inherit. The two identifier
/// spaces diverge for Claude only: harness `"claude-code"` ⇒ runtime `"claude"`.
/// Anything else (notably `"native"`) is already a catalog id.
pub fn runtime_id_for_harness(harness: &str) -> &str {
    match harness {
        "claude-code" => "claude",
        other => other,
    }
}

/// Session parameters inherited from a SPECIFIC runtime's persisted config.
/// This is what a starting session must use: a native session inherits the
/// Native runtime card's model/perm-mode, a claude-code session inherits the
/// Claude card's — NOT whatever happens to be the global `default_agent`.
/// (The historical `session_defaults`, which keyed off `default_agent`, made
/// every native session fall back to the Claude connection because the Claude
/// row's model is what it read.)
pub async fn session_defaults_for(
    store: &Store,
    runtime_id: &str,
) -> anyhow::Result<SessionDefaults> {
    let cfg = get_config(store, runtime_id).await?;
    // Only a model the user explicitly picked is injected into sessions —
    // catalog defaults are UI initial values, not session overrides (an
    // unrecognized id would break the adapter's own default resolution).
    let model = cfg
        .as_ref()
        .and_then(|c| c.model.clone())
        .filter(|m| !m.trim().is_empty());
    let perm_mode = cfg.as_ref().map(|c| ui_perm_to_core(&c.perm_mode));
    Ok(SessionDefaults { model, perm_mode })
}

/// Back-compat entry point that inherits from the global `default_agent`.
/// Prefer [`session_defaults_for`] with the session's own runtime id.
pub async fn session_defaults(store: &Store) -> anyhow::Result<SessionDefaults> {
    let default_id = store
        .get_setting("default_agent")
        .await?
        .unwrap_or_else(|| "claude".to_string());
    session_defaults_for(store, &default_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_version_outputs() {
        assert_eq!(
            parse_version("2.1.4 (Claude Code)").as_deref(),
            Some("2.1.4")
        );
        assert_eq!(parse_version("codex-cli 1.8.2").as_deref(), Some("1.8.2"));
        assert_eq!(
            parse_version("ollama version is 0.6.4").as_deref(),
            Some("0.6.4")
        );
        assert_eq!(parse_version("v0.13.0").as_deref(), Some("0.13.0"));
        assert_eq!(parse_version("no digits here"), None);
        // A bare integer is not a version.
        assert_eq!(parse_version("exit 1"), None);
    }

    #[test]
    fn parses_ollama_list_names() {
        let out = "NAME                    ID              SIZE      MODIFIED\n\
                   qwen3-coder:72b         abc123          40 GB     2 days ago\n\
                   llama4:scout            def456          20 GB     5 weeks ago\n";
        assert_eq!(
            parse_ollama_list(out),
            vec!["qwen3-coder:72b".to_string(), "llama4:scout".to_string()]
        );
        assert!(parse_ollama_list("").is_empty());
    }

    #[test]
    fn ui_perm_maps_to_core_perm() {
        assert_eq!(ui_perm_to_core("plan"), PermMode::Plan);
        assert_eq!(ui_perm_to_core("ask"), PermMode::Default);
        assert_eq!(ui_perm_to_core("edit"), PermMode::AcceptEdits);
        assert_eq!(ui_perm_to_core("full"), PermMode::BypassPermissions);
        assert_eq!(ui_perm_to_core("nonsense"), PermMode::Default);
    }

    #[tokio::test]
    async fn config_upserts_and_tier_merge_over_catalog() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();

        // No rows yet → catalog defaults come back for tiers.
        let tiers = list_tiers(&store, "claude").await.unwrap();
        assert_eq!(tiers.len(), 3);
        assert_eq!(tiers[0].tier_id, "default");
        assert_eq!(tiers[0].value.as_deref(), Some("claude-opus-4-5"));

        // Persist an override for one tier; others keep defaults.
        set_tier(
            &store,
            "claude",
            "fast",
            Some("claude-sonnet-4-5".into()),
            false,
        )
        .await
        .unwrap();
        let tiers = list_tiers(&store, "claude").await.unwrap();
        assert_eq!(
            tiers
                .iter()
                .find(|t| t.tier_id == "fast")
                .unwrap()
                .value
                .as_deref(),
            Some("claude-sonnet-4-5")
        );
        assert_eq!(
            tiers
                .iter()
                .find(|t| t.tier_id == "default")
                .unwrap()
                .value
                .as_deref(),
            Some("claude-opus-4-5")
        );

        // Agent config upsert round-trips.
        upsert_config(
            &store,
            RuntimeConfig {
                id: "claude".into(),
                enabled: true,
                model: Some("claude-sonnet-4-5".into()),
                perm_mode: "edit".into(),
                flags: "--max-turns 40".into(),
            },
        )
        .await
        .unwrap();
        let cfg = get_config(&store, "claude").await.unwrap().unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(list_configs(&store).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn session_defaults_follow_default_agent_config() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();

        // Nothing configured → no overrides injected into sessions.
        let d = session_defaults(&store).await.unwrap();
        assert_eq!(d.model, None);
        assert_eq!(d.perm_mode, None);

        upsert_config(
            &store,
            RuntimeConfig {
                id: "claude".into(),
                enabled: true,
                model: Some("claude-haiku-4-5".into()),
                perm_mode: "full".into(),
                flags: String::new(),
            },
        )
        .await
        .unwrap();
        let d = session_defaults(&store).await.unwrap();
        assert_eq!(d.model.as_deref(), Some("claude-haiku-4-5"));
        assert_eq!(d.perm_mode, Some(PermMode::BypassPermissions));
    }

    #[test]
    fn harness_maps_to_runtime_catalog_id() {
        assert_eq!(runtime_id_for_harness("claude-code"), "claude");
        assert_eq!(runtime_id_for_harness("native"), "native");
        // Unknown harness ids pass through unchanged.
        assert_eq!(runtime_id_for_harness("codex"), "codex");
    }

    #[tokio::test]
    async fn session_defaults_for_reads_the_named_runtime_not_the_default_agent() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();

        // The Native runtime card is configured with a router model, while the
        // (global default) Claude card carries a different one. A native
        // session must inherit the NATIVE model — the old default-agent path
        // wrongly returned the Claude model here, which is why every native
        // turn hit the Claude subscription.
        upsert_config(
            &store,
            RuntimeConfig {
                id: "native".into(),
                enabled: true,
                model: Some("openrouter/deepseek/deepseek-chat:free".into()),
                perm_mode: "edit".into(),
                flags: String::new(),
            },
        )
        .await
        .unwrap();
        upsert_config(
            &store,
            RuntimeConfig {
                id: "claude".into(),
                enabled: true,
                model: Some("claude-opus-4-5".into()),
                perm_mode: "ask".into(),
                flags: String::new(),
            },
        )
        .await
        .unwrap();

        let native = session_defaults_for(&store, "native").await.unwrap();
        assert_eq!(
            native.model.as_deref(),
            Some("openrouter/deepseek/deepseek-chat:free")
        );

        let claude = session_defaults_for(&store, "claude").await.unwrap();
        assert_eq!(claude.model.as_deref(), Some("claude-opus-4-5"));
    }

    #[tokio::test]
    async fn session_defaults_for_treats_blank_model_as_unset() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        upsert_config(
            &store,
            RuntimeConfig {
                id: "native".into(),
                enabled: true,
                model: Some("   ".into()),
                perm_mode: "ask".into(),
                flags: String::new(),
            },
        )
        .await
        .unwrap();
        let d = session_defaults_for(&store, "native").await.unwrap();
        assert_eq!(
            d.model, None,
            "a blank model must not shadow the router default"
        );
    }
}

#[cfg(test)]
mod npm_tests {
    #[test]
    fn npm_argv_targets_latest() {
        assert_eq!(
            super::npm_update_argv("@anthropic-ai/claude-code"),
            vec!["install", "-g", "@anthropic-ai/claude-code@latest"]
        );
    }
}
