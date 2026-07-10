//! `bash` — run a shell command in the session worktree.
//!
//! Always `sh -c` (POSIX) on every platform. Windows has no ambient `sh`,
//! so a cached resolver locates one: `RYUZI_SHELL` env override → `sh.exe`
//! on PATH → Git for Windows (`git.exe`'s sibling `usr\bin\sh.exe`, then
//! the default install roots). No `cmd.exe` fallback — the tool contract
//! is POSIX sh, and model-emitted POSIX syntax must keep meaning what it
//! says. If nothing resolves, the tool returns an actionable error. The
//! resolved shell's directory is also prepended to the child's `PATH` so
//! its sibling POSIX utilities (ls, sleep, grep …) resolve — and shadow
//! same-named Windows tools — even when `usr\bin` is not on the ambient
//! PATH (GUI/PowerShell launches).

use super::{truncate, PermissionSpec, Tool, ToolCtx, ToolOutput};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

const DEFAULT_TIMEOUT_SECS: u64 = 120;
const MAX_TIMEOUT_SECS: u64 = 600;

pub struct Bash;

#[async_trait]
impl Tool for Bash {
    fn name(&self) -> &str {
        "bash"
    }
    fn description(&self) -> &str {
        "Run a shell command with `sh -c` in the working directory. Returns \
         merged stdout and stderr, plus the exit code on failure. Has a \
         timeout (default 120s, max 600s)."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "The shell command to run."},
                "timeout": {"type": "integer", "description": "Timeout in seconds (default 120, max 600)."}
            },
            "required": ["command"]
        })
    }
    fn kind(&self) -> &'static str {
        "execute"
    }
    fn permission(&self, input: &Value) -> PermissionSpec {
        let cmd = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
        let brief: String = cmd.chars().take(80).collect();
        PermissionSpec::new("bash", format!("run: {brief}"))
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let command = input
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("bash: `command` is required"))?;
        let secs = input
            .get("timeout")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .min(MAX_TIMEOUT_SECS);

        #[cfg(windows)]
        let shell: &Path = match resolved_sh() {
            Some(p) => p,
            None => {
                return Ok(ToolOutput::error(
                    "bash: no POSIX shell (`sh`) found on this machine. Install \
                     Git for Windows (https://gitforwindows.org) — its \
                     usr\\bin\\sh.exe is detected automatically — or set the \
                     RYUZI_SHELL environment variable to a POSIX sh-compatible \
                     executable, then restart the app.",
                ));
            }
        };
        #[cfg(not(windows))]
        let shell: &Path = Path::new("sh");

        let mut cmd = tokio::process::Command::new(shell);
        cmd.arg("-c")
            .arg(command)
            .current_dir(&ctx.work_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // The resolved sh often lives in Git\usr\bin, which is NOT on the
        // ambient PATH in GUI/PowerShell contexts — without it the shell
        // starts but external POSIX utilities (ls, sleep, grep …) are
        // missing. Prepend so POSIX coreutils shadow same-named Windows
        // tools (sort, find), matching what a Git Bash login shell does.
        #[cfg(windows)]
        if let Some(dir) = shell.parent() {
            let ambient = std::env::var_os("PATH").unwrap_or_default();
            let mut dirs: Vec<PathBuf> = std::env::split_paths(&ambient).collect();
            if !dirs.iter().any(|d| d == dir) {
                dirs.insert(0, dir.to_path_buf());
                if let Ok(joined) = std::env::join_paths(dirs) {
                    cmd.env("PATH", joined);
                }
            }
        }
        // Repo-wide Windows spawn convention (see crate::process_util):
        // never flash a console window when the Cockpit GUI runs a tool call.
        crate::process_util::no_window(&mut cmd);

        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return Ok(ToolOutput::error(format!("bash: failed to spawn: {e}"))),
        };

        let output = tokio::select! {
            // Cancellation drops the wait future; kill_on_drop reaps the child.
            _ = ctx.cancel.cancelled() => {
                return Ok(ToolOutput::error("bash: interrupted"));
            }
            res = tokio::time::timeout(Duration::from_secs(secs), child.wait_with_output()) => {
                match res {
                    Ok(Ok(o)) => o,
                    Ok(Err(e)) => return Ok(ToolOutput::error(format!("bash: {e}"))),
                    Err(_) => return Ok(ToolOutput::error(format!(
                        "bash: timed out after {secs}s"
                    ))),
                }
            }
        };

        let mut text = String::new();
        text.push_str(&String::from_utf8_lossy(&output.stdout));
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.trim().is_empty() {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&stderr);
        }
        let is_error = !output.status.success();
        if is_error {
            let code = output
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".to_string());
            text.push_str(&format!("\n[exit code {code}]"));
        }
        let text = truncate(&text, &ctx.caps);
        Ok(ToolOutput {
            for_model: text,
            model_blocks: None,
            display: exit_display(output.status.code()),
            is_error,
        })
    }
}

/// Structured display extras for a finished process: `{"exit_code": N}` when
/// the OS reported a code (0 included — the UI renders it as a badge), `None`
/// for signal deaths. The model-facing text keeps its `[exit code N]` suffix
/// on failure; this field exists so the UI never parses that text.
fn exit_display(code: Option<i32>) -> Option<Value> {
    code.map(|c| json!({ "exit_code": c }))
}

/// Locate a POSIX `sh` for Windows. Pure — the caller passes the relevant
/// environment values in — so precedence is unit-testable on every platform
/// without mutating process env (tests run in parallel).
///
/// Order (first hit wins):
/// 1. `ryuzi_shell` (the `RYUZI_SHELL` env override), honored only when it
///    points at an existing file; a stale override falls through.
/// 2. `sh.exe` found in a `PATH` directory (Git Bash sessions, MSYS2).
/// 3. Git for Windows discovery: a `PATH` directory containing `git.exe`
///    (`Git\cmd` and `Git\bin` are both direct children of the install
///    root), probing the sibling `<root>\usr\bin\sh.exe`; then the common
///    install roots `<ProgramFiles>\Git` and `<LocalAppData>\Programs\Git`.
///
/// `None` means no shell — the tool reports an actionable error. There is
/// deliberately NO `cmd.exe` fallback: the tool contract is `sh -c`, and
/// model-emitted POSIX syntax must keep meaning what it says.
#[cfg_attr(not(windows), allow(dead_code))]
fn resolve_sh_with(
    ryuzi_shell: Option<PathBuf>,
    path_var: Option<OsString>,
    program_files: Option<PathBuf>,
    local_app_data: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(p) = ryuzi_shell {
        if p.is_file() {
            return Some(p);
        }
    }

    let path_dirs: Vec<PathBuf> = path_var
        .as_deref()
        .map(|v| std::env::split_paths(v).collect())
        .unwrap_or_default();

    for dir in &path_dirs {
        let sh = dir.join("sh.exe");
        if sh.is_file() {
            return Some(sh);
        }
    }

    for dir in &path_dirs {
        if dir.join("git.exe").is_file() {
            if let Some(root) = dir.parent() {
                let sh = root.join("usr").join("bin").join("sh.exe");
                if sh.is_file() {
                    return Some(sh);
                }
            }
        }
    }

    let install_roots = [
        program_files.map(|p| p.join("Git")),
        local_app_data.map(|p| p.join("Programs").join("Git")),
    ];
    for root in install_roots.into_iter().flatten() {
        let sh = root.join("usr").join("bin").join("sh.exe");
        if sh.is_file() {
            return Some(sh);
        }
    }

    None
}

/// Windows: the POSIX shell the `bash` tool spawns, resolved once per
/// process and cached (a `OnceLock` — changing the environment requires an
/// app restart). `None` = nothing found; the tool returns an actionable
/// error instead of spawning.
#[cfg(windows)]
fn resolved_sh() -> Option<&'static Path> {
    static SH: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    SH.get_or_init(|| {
        resolve_sh_with(
            std::env::var_os("RYUZI_SHELL").map(PathBuf::from),
            std::env::var_os("PATH"),
            std::env::var_os("ProgramFiles").map(PathBuf::from),
            std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
        )
    })
    .as_deref()
}

#[cfg(test)]
mod tests {
    use super::super::testutil::ctx_at;
    use super::*;

    #[tokio::test]
    async fn runs_command_in_workdir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("marker.txt"), "").unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Bash.execute(&ctx, json!({"command": "ls"})).await.unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert!(out.for_model.contains("marker.txt"));
    }

    #[tokio::test]
    async fn nonzero_exit_is_a_tool_error_with_code() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Bash
            .execute(&ctx, json!({"command": "exit 3"}))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.for_model.contains("exit code 3"));
        assert_eq!(out.display, Some(json!({ "exit_code": 3 })));
    }

    #[tokio::test]
    async fn timeout_is_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Bash
            .execute(&ctx, json!({"command": "sleep 5", "timeout": 1}))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.for_model.contains("timed out"));
    }

    #[tokio::test]
    async fn cancel_interrupts() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await;
        ctx.cancel.cancel();
        let out = Bash
            .execute(&ctx, json!({"command": "sleep 5"}))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.for_model.contains("interrupted"));
    }

    #[test]
    fn exit_display_is_structured_and_keeps_zero() {
        assert_eq!(exit_display(Some(0)), Some(json!({ "exit_code": 0 })));
        assert_eq!(exit_display(Some(3)), Some(json!({ "exit_code": 3 })));
    }

    #[test]
    fn exit_display_is_absent_for_signal_deaths() {
        assert_eq!(exit_display(None), None);
    }

    /// End-to-end on Windows: the resolver finds a shell even when `sh` is
    /// not on the ambient PATH (e.g. tests launched from PowerShell, or the
    /// Cockpit GUI), and the tool runs a command through it.
    #[cfg(windows)]
    #[tokio::test]
    async fn windows_resolves_a_shell_and_runs_echo() {
        let Some(sh) = resolved_sh() else {
            // Machine without Git for Windows / RYUZI_SHELL: nothing to test.
            eprintln!("skipping: no POSIX sh resolvable on this machine");
            return;
        };
        assert!(sh.is_file(), "resolved sh missing: {}", sh.display());
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Bash
            .execute(&ctx, json!({"command": "echo hi"}))
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert!(out.for_model.contains("hi"));
    }
}

#[cfg(test)]
mod resolver_tests {
    use super::resolve_sh_with;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    /// Create an empty file at `p`, creating parent dirs as needed.
    fn touch(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "").unwrap();
    }

    /// Build a PATH-style value (`;`-joined on Windows, `:` elsewhere).
    fn path_var(dirs: &[PathBuf]) -> Option<OsString> {
        Some(std::env::join_paths(dirs.iter().cloned()).unwrap())
    }

    #[test]
    fn env_override_wins_when_it_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let custom = tmp.path().join("custom-shell.exe");
        touch(&custom);
        // Even with an sh.exe available on PATH, the override wins.
        let bin = tmp.path().join("bin");
        touch(&bin.join("sh.exe"));
        let got = resolve_sh_with(Some(custom.clone()), path_var(&[bin]), None, None);
        assert_eq!(got, Some(custom));
    }

    #[test]
    fn env_override_pointing_nowhere_falls_through_to_path() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("bin");
        let sh = bin.join("sh.exe");
        touch(&sh);
        let got = resolve_sh_with(
            Some(tmp.path().join("missing-shell.exe")),
            path_var(&[bin]),
            None,
            None,
        );
        assert_eq!(got, Some(sh));
    }

    #[test]
    fn path_sh_beats_git_sibling_probe() {
        let tmp = tempfile::tempdir().unwrap();
        // A Git install whose cmd dir is on PATH (git.exe + sibling sh.exe)...
        let git_cmd = tmp.path().join("Git").join("cmd");
        touch(&git_cmd.join("git.exe"));
        touch(
            &tmp.path()
                .join("Git")
                .join("usr")
                .join("bin")
                .join("sh.exe"),
        );
        // ...and a direct sh.exe later on PATH. The direct PATH hit wins.
        let bin = tmp.path().join("bin");
        let sh = bin.join("sh.exe");
        touch(&sh);
        let got = resolve_sh_with(None, path_var(&[git_cmd, bin]), None, None);
        assert_eq!(got, Some(sh));
    }

    #[test]
    fn git_cmd_dir_on_path_probes_sibling_usr_bin() {
        let tmp = tempfile::tempdir().unwrap();
        let git_cmd = tmp.path().join("Git").join("cmd");
        touch(&git_cmd.join("git.exe"));
        let git_sh = tmp
            .path()
            .join("Git")
            .join("usr")
            .join("bin")
            .join("sh.exe");
        touch(&git_sh);
        let got = resolve_sh_with(None, path_var(&[git_cmd]), None, None);
        assert_eq!(got, Some(git_sh));
    }

    #[test]
    fn git_bin_dir_on_path_probes_sibling_usr_bin() {
        let tmp = tempfile::tempdir().unwrap();
        let git_bin = tmp.path().join("Git").join("bin");
        touch(&git_bin.join("git.exe"));
        let git_sh = tmp
            .path()
            .join("Git")
            .join("usr")
            .join("bin")
            .join("sh.exe");
        touch(&git_sh);
        let got = resolve_sh_with(None, path_var(&[git_bin]), None, None);
        assert_eq!(got, Some(git_sh));
    }

    #[test]
    fn program_files_root_is_probed() {
        let tmp = tempfile::tempdir().unwrap();
        let pf = tmp.path().join("Program Files");
        let sh = pf.join("Git").join("usr").join("bin").join("sh.exe");
        touch(&sh);
        let got = resolve_sh_with(None, None, Some(pf), None);
        assert_eq!(got, Some(sh));
    }

    #[test]
    fn local_app_data_root_is_probed() {
        let tmp = tempfile::tempdir().unwrap();
        let lad = tmp.path().join("AppData").join("Local");
        let sh = lad
            .join("Programs")
            .join("Git")
            .join("usr")
            .join("bin")
            .join("sh.exe");
        touch(&sh);
        let got = resolve_sh_with(None, None, None, Some(lad));
        assert_eq!(got, Some(sh));
    }

    #[test]
    fn nothing_found_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let empty = tmp.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let got = resolve_sh_with(None, path_var(&[empty]), None, None);
        assert_eq!(got, None);
    }
}
