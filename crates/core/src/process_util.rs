//! Shared child-process conventions.
//!
//! On Windows, a GUI process (the Cockpit desktop app) that spawns a console
//! subprocess flashes a visible console window unless the spawn sets the
//! `CREATE_NO_WINDOW` creation flag. Every captured-output spawn in the
//! engine goes through these helpers. Interactive spawns must NOT use them:
//! PTY terminals (portable_pty/ConPTY) and "open in terminal" launchers open
//! windows on purpose.

/// Windows `CREATE_NO_WINDOW` process-creation flag.
#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Suppress the console window for a tokio `Command` (no-op off Windows).
pub fn no_window(cmd: &mut tokio::process::Command) -> &mut tokio::process::Command {
    #[cfg(windows)]
    {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Suppress the console window for a std `Command` (no-op off Windows).
pub fn no_window_std(cmd: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Locate `bin` on PATH, honoring PATHEXT on Windows (npm shims install
/// `pwsh.cmd` etc., not `.exe`).
pub fn find_on_path(bin: &str) -> Option<std::path::PathBuf> {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The helpers must be chainable and still spawn a working process.
    #[tokio::test]
    async fn no_window_commands_still_run() {
        let mut cmd = tokio::process::Command::new("git");
        cmd.arg("--version");
        let out = no_window(&mut cmd).output().await.unwrap();
        assert!(out.status.success());

        let mut cmd = std::process::Command::new("git");
        cmd.arg("--version");
        let out = no_window_std(&mut cmd).output().unwrap();
        assert!(out.status.success());
    }
}
