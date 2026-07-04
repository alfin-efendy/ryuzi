//! Apply/reset native CLI-tool configs so runtimes talk to Ryuzi's endpoint.
//! Ported from 9router (MIT, (c) 2024-2026 decolua and contributors) —
//! pattern and key names from src/app/api/cli-tools/*-settings/route.js:
//! detect → tolerant read → merge ONLY our keys → write back → surgical reset.
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

pub struct EndpointInfo {
    /// Origin without /v1 — Claude Code appends /v1/messages itself.
    pub base_url: String,
    pub api_key: String,
}

pub struct RuntimeMapping {
    pub model: String,
    pub opus: Option<String>,
    pub sonnet: Option<String>,
    pub haiku: Option<String>,
    pub models: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConfigStatus {
    pub config_path: String,
    pub exists: bool,
    pub configured: bool,
}

/// Remove `,` directly before `}` / `]` outside of strings (JSONC tolerance).
pub fn strip_trailing_commas(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_str = false;
    let mut escaped = false;
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if in_str {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                out.push(c);
            }
            ',' => {
                // Peek the next non-whitespace char.
                let next = chars[i + 1..].iter().find(|ch| !ch.is_whitespace());
                if !matches!(next, Some('}') | Some(']')) {
                    out.push(c);
                }
            }
            _ => out.push(c),
        }
    }
    out
}

fn read_json(path: &Path) -> anyhow::Result<Value> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let raw = std::fs::read_to_string(path)?;
    if raw.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&strip_trailing_commas(&raw))
        .map_err(|e| anyhow::anyhow!("refusing to modify unparseable config {}: {e}", path.display()))
}

fn write_json(path: &Path, v: &Value) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(v)?)?;
    Ok(())
}

fn obj<'a>(v: &'a mut Value, key: &str) -> &'a mut Map<String, Value> {
    if !v[key].is_object() {
        v[key] = json!({});
    }
    v[key].as_object_mut().unwrap()
}

// ---------------------------------------------------------------------------
// Claude Code — ~/.claude/settings.json (env block)
// ---------------------------------------------------------------------------

const CLAUDE_ENV_KEYS: &[&str] = &[
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
];

fn claude_path(home: &Path) -> PathBuf {
    home.join(".claude").join("settings.json")
}

pub fn claude_status(home: &Path) -> anyhow::Result<ConfigStatus> {
    let path = claude_path(home);
    let exists = path.exists();
    let configured = if exists {
        let v = read_json(&path)?;
        v["env"]["ANTHROPIC_BASE_URL"]
            .as_str()
            .map(|u| u.starts_with("http://127.0.0.1:"))
            .unwrap_or(false)
    } else {
        false
    };
    Ok(ConfigStatus { config_path: path.display().to_string(), exists, configured })
}

pub fn claude_apply(home: &Path, ep: &EndpointInfo, m: &RuntimeMapping) -> anyhow::Result<()> {
    let path = claude_path(home);
    let mut v = read_json(&path)?;
    let env = obj(&mut v, "env");
    env.insert("ANTHROPIC_BASE_URL".into(), json!(ep.base_url));
    env.insert("ANTHROPIC_AUTH_TOKEN".into(), json!(ep.api_key));
    if let Some(x) = &m.opus {
        env.insert("ANTHROPIC_DEFAULT_OPUS_MODEL".into(), json!(x));
    }
    if let Some(x) = &m.sonnet {
        env.insert("ANTHROPIC_DEFAULT_SONNET_MODEL".into(), json!(x));
    }
    if let Some(x) = &m.haiku {
        env.insert("ANTHROPIC_DEFAULT_HAIKU_MODEL".into(), json!(x));
    }
    write_json(&path, &v)
}

pub fn claude_reset(home: &Path) -> anyhow::Result<()> {
    let path = claude_path(home);
    if !path.exists() {
        return Ok(());
    }
    let mut v = read_json(&path)?;
    if v["env"].is_object() {
        let env = v["env"].as_object_mut().unwrap();
        for k in CLAUDE_ENV_KEYS {
            env.remove(*k);
        }
        if env.is_empty() {
            v.as_object_mut().unwrap().remove("env");
        }
    }
    write_json(&path, &v)
}

// ---------------------------------------------------------------------------
// Codex — ~/.codex/config.toml + ~/.codex/auth.json
// ---------------------------------------------------------------------------

fn codex_cfg_path(home: &Path) -> PathBuf {
    home.join(".codex").join("config.toml")
}

pub fn codex_status(home: &Path) -> anyhow::Result<ConfigStatus> {
    let path = codex_cfg_path(home);
    let exists = path.exists();
    let configured = if exists {
        let doc: toml_edit::DocumentMut = std::fs::read_to_string(&path)?
            .parse()
            .map_err(|e| anyhow::anyhow!("unparseable {}: {e}", path.display()))?;
        doc.get("model_provider").and_then(|v| v.as_str()) == Some("ryuzi")
    } else {
        false
    };
    Ok(ConfigStatus { config_path: path.display().to_string(), exists, configured })
}

pub fn codex_apply(home: &Path, ep: &EndpointInfo, m: &RuntimeMapping) -> anyhow::Result<()> {
    let path = codex_cfg_path(home);
    let mut doc: toml_edit::DocumentMut = if path.exists() {
        std::fs::read_to_string(&path)?
            .parse()
            .map_err(|e| anyhow::anyhow!("refusing to modify unparseable {}: {e}", path.display()))?
    } else {
        toml_edit::DocumentMut::new()
    };
    doc["model"] = toml_edit::value(m.model.clone());
    doc["model_provider"] = toml_edit::value("ryuzi");
    let mut tbl = toml_edit::Table::new();
    tbl["name"] = toml_edit::value("Ryuzi");
    tbl["base_url"] = toml_edit::value(format!("{}/v1", ep.base_url));
    // F1 serves Chat Completions only; switch to "responses" when F2 adds /v1/responses.
    tbl["wire_api"] = toml_edit::value("chat");
    tbl["env_key"] = toml_edit::value("OPENAI_API_KEY");
    if !doc.contains_key("model_providers") {
        doc["model_providers"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    doc["model_providers"]["ryuzi"] = toml_edit::Item::Table(tbl);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, doc.to_string())?;

    // auth.json: OPENAI_API_KEY so codex sends our key to the ryuzi provider.
    let auth_path = home.join(".codex").join("auth.json");
    let mut auth = read_json(&auth_path)?;
    auth.as_object_mut()
        .unwrap()
        .insert("OPENAI_API_KEY".into(), json!(ep.api_key));
    write_json(&auth_path, &auth)
}

pub fn codex_reset(home: &Path) -> anyhow::Result<()> {
    let path = codex_cfg_path(home);
    if path.exists() {
        let mut doc: toml_edit::DocumentMut = std::fs::read_to_string(&path)?
            .parse()
            .map_err(|e| anyhow::anyhow!("unparseable {}: {e}", path.display()))?;
        if doc.get("model_provider").and_then(|v| v.as_str()) == Some("ryuzi") {
            doc.remove("model_provider");
            doc.remove("model");
        }
        if let Some(mp) = doc.get_mut("model_providers").and_then(|i| i.as_table_mut()) {
            mp.remove("ryuzi");
            if mp.is_empty() {
                doc.remove("model_providers");
            }
        }
        std::fs::write(&path, doc.to_string())?;
    }
    let auth_path = home.join(".codex").join("auth.json");
    if auth_path.exists() {
        let mut auth = read_json(&auth_path)?;
        let is_ours = auth["OPENAI_API_KEY"].as_str().map(|k| k.starts_with("ryz-")).unwrap_or(false);
        if is_ours {
            auth.as_object_mut().unwrap().remove("OPENAI_API_KEY");
            write_json(&auth_path, &auth)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// OpenCode — ~/.config/opencode/opencode.json
// ---------------------------------------------------------------------------

fn opencode_path(home: &Path) -> PathBuf {
    home.join(".config").join("opencode").join("opencode.json")
}

pub fn opencode_status(home: &Path) -> anyhow::Result<ConfigStatus> {
    let path = opencode_path(home);
    let exists = path.exists();
    let configured = if exists {
        read_json(&path)?["provider"]["ryuzi"].is_object()
    } else {
        false
    };
    Ok(ConfigStatus { config_path: path.display().to_string(), exists, configured })
}

pub fn opencode_apply(home: &Path, ep: &EndpointInfo, m: &RuntimeMapping) -> anyhow::Result<()> {
    let path = opencode_path(home);
    let mut v = read_json(&path)?;
    let mut models = Map::new();
    for model in &m.models {
        models.insert(model.clone(), json!({}));
    }
    if models.is_empty() {
        models.insert(m.model.clone(), json!({}));
    }
    obj(&mut v, "provider").insert(
        "ryuzi".into(),
        json!({
            "npm": "@ai-sdk/openai-compatible",
            "name": "Ryuzi",
            "options": {"baseURL": format!("{}/v1", ep.base_url), "apiKey": ep.api_key},
            "models": models,
        }),
    );
    v.as_object_mut()
        .unwrap()
        .insert("model".into(), json!(format!("ryuzi/{}", m.model)));
    write_json(&path, &v)
}

pub fn opencode_reset(home: &Path) -> anyhow::Result<()> {
    let path = opencode_path(home);
    if !path.exists() {
        return Ok(());
    }
    let mut v = read_json(&path)?;
    if v["provider"].is_object() {
        let p = v["provider"].as_object_mut().unwrap();
        p.remove("ryuzi");
        if p.is_empty() {
            v.as_object_mut().unwrap().remove("provider");
        }
    }
    let ours = v["model"].as_str().map(|s| s.starts_with("ryuzi/")).unwrap_or(false);
    if ours {
        v.as_object_mut().unwrap().remove("model");
    }
    write_json(&path, &v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ep() -> EndpointInfo {
        EndpointInfo { base_url: "http://127.0.0.1:21128".into(), api_key: "ryz-abc".into() }
    }

    fn mapping() -> RuntimeMapping {
        RuntimeMapping {
            model: "anthropic/claude-sonnet-4-5".into(),
            opus: Some("anthropic/claude-opus-4-5".into()),
            sonnet: Some("anthropic/claude-sonnet-4-5".into()),
            haiku: Some("anthropic/claude-haiku-4-5".into()),
            models: vec!["anthropic/claude-sonnet-4-5".into()],
        }
    }

    #[test]
    fn strip_trailing_commas_respects_strings() {
        assert_eq!(strip_trailing_commas(r#"{"a": 1,}"#), r#"{"a": 1}"#);
        assert_eq!(strip_trailing_commas(r#"{"a": [1, 2,],}"#), r#"{"a": [1, 2]}"#);
        // commas inside strings survive
        assert_eq!(strip_trailing_commas(r#"{"a": "x,}"}"#), r#"{"a": "x,}"}"#);
    }

    #[test]
    fn claude_apply_merges_and_reset_is_surgical() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".claude/settings.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"env": {"KEEP": "1"}, "theme": "dark",}"#).unwrap();

        let st = claude_status(home.path()).unwrap();
        assert!(st.exists && !st.configured);

        claude_apply(home.path(), &ep(), &mapping()).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:21128");
        assert_eq!(v["env"]["ANTHROPIC_AUTH_TOKEN"], "ryz-abc");
        assert_eq!(v["env"]["ANTHROPIC_DEFAULT_OPUS_MODEL"], "anthropic/claude-opus-4-5");
        assert_eq!(v["env"]["KEEP"], "1");       // user's env preserved
        assert_eq!(v["theme"], "dark");           // user's settings preserved
        assert!(claude_status(home.path()).unwrap().configured);

        claude_reset(home.path()).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(v["env"].get("ANTHROPIC_BASE_URL").is_none());
        assert_eq!(v["env"]["KEEP"], "1");
        assert!(!claude_status(home.path()).unwrap().configured);
    }

    #[test]
    fn claude_apply_creates_file_when_absent() {
        let home = tempfile::tempdir().unwrap();
        assert!(!claude_status(home.path()).unwrap().exists);
        claude_apply(home.path(), &ep(), &mapping()).unwrap();
        assert!(claude_status(home.path()).unwrap().configured);
    }

    #[test]
    fn claude_apply_refuses_to_clobber_corrupt_config() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".claude/settings.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json at all").unwrap();
        assert!(claude_apply(home.path(), &ep(), &mapping()).is_err());
        // untouched
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json at all");
    }

    #[test]
    fn codex_apply_merges_toml_and_auth_and_resets() {
        let home = tempfile::tempdir().unwrap();
        let cfg = home.path().join(".codex/config.toml");
        std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
        std::fs::write(&cfg, "# user comment\nsandbox_mode = \"workspace-write\"\n").unwrap();

        codex_apply(home.path(), &ep(), &mapping()).unwrap();
        let text = std::fs::read_to_string(&cfg).unwrap();
        assert!(text.contains("# user comment"), "toml_edit must preserve comments");
        assert!(text.contains("model_provider = \"ryuzi\""));
        assert!(text.contains("base_url = \"http://127.0.0.1:21128/v1\""));
        assert!(text.contains("wire_api = \"chat\""));
        let auth: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home.path().join(".codex/auth.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(auth["OPENAI_API_KEY"], "ryz-abc");
        assert!(codex_status(home.path()).unwrap().configured);

        codex_reset(home.path()).unwrap();
        let text = std::fs::read_to_string(&cfg).unwrap();
        assert!(!text.contains("ryuzi"));
        assert!(text.contains("sandbox_mode"));
        let auth: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home.path().join(".codex/auth.json")).unwrap(),
        )
        .unwrap();
        assert!(auth.get("OPENAI_API_KEY").is_none());
    }

    #[test]
    fn opencode_apply_and_reset() {
        let home = tempfile::tempdir().unwrap();
        opencode_apply(home.path(), &ep(), &mapping()).unwrap();
        let path = home.path().join(".config/opencode/opencode.json");
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["provider"]["ryuzi"]["npm"], "@ai-sdk/openai-compatible");
        assert_eq!(v["provider"]["ryuzi"]["options"]["baseURL"], "http://127.0.0.1:21128/v1");
        assert_eq!(v["provider"]["ryuzi"]["options"]["apiKey"], "ryz-abc");
        assert_eq!(v["model"], "ryuzi/anthropic/claude-sonnet-4-5");
        assert!(v["provider"]["ryuzi"]["models"]["anthropic/claude-sonnet-4-5"].is_object());
        assert!(opencode_status(home.path()).unwrap().configured);

        opencode_reset(home.path()).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(v["provider"].get("ryuzi").is_none());
        assert!(v.get("model").is_none());
        let _ = json!(null); // keep serde_json::json import used
    }
}
