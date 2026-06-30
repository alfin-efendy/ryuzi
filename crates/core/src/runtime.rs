use crate::domain::{AgentEvent, PermMode};
use crate::policy::tool_summary;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub trait ClaudeRunner: Send + Sync + 'static {
    fn spawn(
        &self,
        args: Vec<String>,
        cwd: PathBuf,
        env: Vec<(String, String)>,
        cancel: CancellationToken,
    ) -> mpsc::Receiver<Result<String, String>>;
}

pub fn resolve_claude_binary() -> String {
    if let Ok(path) = which_claude() {
        return path;
    }
    if let Ok(home) = std::env::var("HOME") {
        for p in [
            format!("{home}/.local/bin/claude"),
            format!("{home}/.bun/bin/claude"),
        ] {
            if Path::new(&p).exists() {
                return p;
            }
        }
    }
    for p in ["/usr/local/bin/claude", "/opt/homebrew/bin/claude"] {
        if Path::new(p).exists() {
            return p.to_string();
        }
    }
    "claude".to_string()
}

fn which_claude() -> Result<String, ()> {
    let path_var = std::env::var("PATH").map_err(|_| ())?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join("claude");
        if candidate.exists() {
            return Ok(candidate.to_string_lossy().into_owned());
        }
    }
    Err(())
}

pub struct ProcessRunner;

impl ClaudeRunner for ProcessRunner {
    fn spawn(
        &self,
        args: Vec<String>,
        cwd: PathBuf,
        env: Vec<(String, String)>,
        cancel: CancellationToken,
    ) -> mpsc::Receiver<Result<String, String>> {
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(async move {
            let mut cmd = tokio::process::Command::new(resolve_claude_binary());
            cmd.args(&args)
                .current_dir(&cwd)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            for (k, v) in env {
                cmd.env(k, v);
            }
            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(Err(format!("spawn failed: {e}"))).await;
                    return;
                }
            };
            let stdout = match child.stdout.take() {
                Some(s) => s,
                None => {
                    let _ = tx.send(Err("no stdout".into())).await;
                    return;
                }
            };
            if let Some(stderr) = child.stderr.take() {
                tokio::spawn(async move {
                    let mut lines = BufReader::new(stderr).lines();
                    while let Ok(Some(_)) = lines.next_line().await {}
                });
            }
            let mut lines = BufReader::new(stdout).lines();
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        let _ = child.kill().await;
                        break;
                    }
                    next = lines.next_line() => {
                        match next {
                            Ok(Some(line)) => {
                                if tx.send(Ok(line)).await.is_err() { break; }
                            }
                            Ok(None) => break,
                            Err(e) => { let _ = tx.send(Err(e.to_string())).await; break; }
                        }
                    }
                }
            }
            let _ = child.wait().await;
        });
        rx
    }
}

pub struct ApprovalWiring {
    pub url: String,
    pub session_pk: String,
    pub hook_bin_path: String,
}

pub struct RunInput {
    pub workdir: PathBuf,
    pub resume: Option<String>,
    pub prompt: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub permission_mode: PermMode,
    pub approval: Option<ApprovalWiring>,
}

pub fn build_hook_settings(hook_bin_path: &str) -> String {
    serde_json::json!({
        "hooks": {
            "PreToolUse": [
                { "matcher": "*", "hooks": [ { "type": "command", "command": hook_bin_path } ] }
            ]
        }
    })
    .to_string()
}

pub fn build_claude_args(input: &RunInput, new_session_id: &str) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-p".into(),
        input.prompt.clone(),
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
    ];
    match &input.resume {
        Some(r) => {
            args.push("--resume".into());
            args.push(r.clone());
        }
        None => {
            args.push("--session-id".into());
            args.push(new_session_id.to_string());
        }
    }
    if let Some(m) = &input.model {
        args.push("--model".into());
        args.push(m.clone());
    }
    if let Some(e) = &input.effort {
        args.push("--effort".into());
        args.push(e.clone());
    }
    args.push("--permission-mode".into());
    args.push(input.permission_mode.as_str().into());
    if input.permission_mode == PermMode::Default {
        if let Some(a) = &input.approval {
            args.push("--settings".into());
            args.push(build_hook_settings(&a.hook_bin_path));
        }
    }
    args
}

pub fn parse_line(line: &str) -> Vec<AgentEvent> {
    let v: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    match v.get("type").and_then(|t| t.as_str()) {
        Some("system") => {
            if v.get("subtype").and_then(|s| s.as_str()) == Some("init") {
                let sid = v.get("session_id").and_then(|s| s.as_str()).unwrap_or("").to_string();
                vec![AgentEvent::Init { session_id: sid }]
            } else {
                vec![]
            }
        }
        Some("assistant") => {
            let mut out = Vec::new();
            if let Some(content) = v
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())
            {
                for b in content {
                    match b.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                                out.push(AgentEvent::Text { text: t.to_string() });
                            }
                        }
                        Some("tool_use") => {
                            let name = b.get("name").and_then(|n| n.as_str()).unwrap_or("");
                            let input = b.get("input").cloned().unwrap_or(serde_json::Value::Null);
                            out.push(AgentEvent::Status { text: tool_summary(name, &input) });
                        }
                        _ => {}
                    }
                }
            }
            out
        }
        Some("result") => {
            if v.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false) {
                let msg = v
                    .get("result")
                    .and_then(|r| r.as_str())
                    .or_else(|| v.get("subtype").and_then(|s| s.as_str()))
                    .unwrap_or("error")
                    .to_string();
                vec![AgentEvent::Error { message: msg }]
            } else {
                let sid = v.get("session_id").and_then(|s| s.as_str()).map(|s| s.to_string());
                vec![AgentEvent::Result { session_id: sid }]
            }
        }
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{AgentEvent, PermMode};
    use std::path::PathBuf;

    fn base_input() -> RunInput {
        RunInput {
            workdir: PathBuf::from("/tmp"),
            resume: None,
            prompt: "hi".into(),
            model: Some("opus".into()),
            effort: None,
            permission_mode: PermMode::Default,
            approval: None,
        }
    }

    #[test]
    fn args_include_stream_json_and_session_id() {
        let a = build_claude_args(&base_input(), "sid-1");
        assert!(a.windows(2).any(|w| w == ["--output-format", "stream-json"]));
        assert!(a.windows(2).any(|w| w == ["--session-id", "sid-1"]));
        assert!(a.windows(2).any(|w| w == ["--model", "opus"]));
        assert!(a.windows(2).any(|w| w == ["--permission-mode", "default"]));
    }

    #[test]
    fn args_resume_replaces_session_id() {
        let mut i = base_input();
        i.resume = Some("prev".into());
        let a = build_claude_args(&i, "ignored");
        assert!(a.windows(2).any(|w| w == ["--resume", "prev"]));
        assert!(!a.iter().any(|s| s == "--session-id"));
    }

    #[test]
    fn parse_assistant_text_and_tool_use() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hello"},{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#;
        let evs = parse_line(line);
        assert_eq!(evs[0], AgentEvent::Text { text: "hello".into() });
        assert_eq!(evs[1], AgentEvent::Status { text: "Bash: ls".into() });
    }

    #[test]
    fn parse_system_init_and_result_and_error() {
        assert_eq!(
            parse_line(r#"{"type":"system","subtype":"init","session_id":"abc"}"#),
            vec![AgentEvent::Init { session_id: "abc".into() }]
        );
        assert_eq!(
            parse_line(r#"{"type":"result","session_id":"abc"}"#),
            vec![AgentEvent::Result { session_id: Some("abc".into()) }]
        );
        assert_eq!(
            parse_line(r#"{"type":"result","is_error":true,"result":"boom"}"#),
            vec![AgentEvent::Error { message: "boom".into() }]
        );
        assert_eq!(parse_line("not json"), Vec::<AgentEvent>::new());
    }

    use tokio_util::sync::CancellationToken;

    struct FakeRunner {
        lines: Vec<String>,
    }
    impl ClaudeRunner for FakeRunner {
        fn spawn(
            &self,
            _args: Vec<String>,
            _cwd: std::path::PathBuf,
            _env: Vec<(String, String)>,
            _cancel: CancellationToken,
        ) -> tokio::sync::mpsc::Receiver<Result<String, String>> {
            let (tx, rx) = tokio::sync::mpsc::channel(16);
            let lines = self.lines.clone();
            tokio::spawn(async move {
                for l in lines {
                    if tx.send(Ok(l)).await.is_err() {
                        break;
                    }
                }
            });
            rx
        }
    }

    #[test]
    fn hook_settings_has_pretooluse_command() {
        let s = build_hook_settings("/path/to/ryuzi-hook");
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["hooks"]["PreToolUse"][0]["hooks"][0]["command"], "/path/to/ryuzi-hook");
        assert_eq!(v["hooks"]["PreToolUse"][0]["matcher"], "*");
    }

    #[tokio::test]
    async fn runner_trait_streams_lines() {
        let runner = FakeRunner { lines: vec!["a".into(), "b".into()] };
        let mut rx = runner.spawn(vec![], "/tmp".into(), vec![], CancellationToken::new());
        let mut got = Vec::new();
        while let Some(item) = rx.recv().await {
            got.push(item.unwrap());
        }
        assert_eq!(got, vec!["a".to_string(), "b".to_string()]);
    }
}
