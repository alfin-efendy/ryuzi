#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos")),
    allow(unused_imports)
)]
use std::path::PathBuf;
use std::process::Command;

use crate::dispatch::Deps;

pub fn cmd_service(args: &[String], deps: &mut Deps) -> u8 {
    match args.first().map(String::as_str) {
        Some("install") => install(deps),
        Some("uninstall") => uninstall(deps),
        Some("status") => status(deps),
        _ => {
            (deps.err)("usage: ryuzi service <install|uninstall|status>");
            1
        }
    }
}

/// systemd user unit: foreground `ryuzi start`, restart on failure.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
pub(crate) fn systemd_unit(exec: &str) -> String {
    format!(
        "[Unit]\n\
         Description=Ryuzi engine daemon\n\
         After=network-online.target\n\
         \n\
         [Service]\n\
         ExecStart={exec} start\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n"
    )
}

/// launchd user agent: KeepAlive so crashes restart it, like Restart=on-failure.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
pub(crate) fn launchd_plist(exec: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>Label</key>\n\
         \t<string>dev.ryuzi.daemon</string>\n\
         \t<key>ProgramArguments</key>\n\
         \t<array>\n\
         \t\t<string>{exec}</string>\n\
         \t\t<string>start</string>\n\
         \t</array>\n\
         \t<key>KeepAlive</key>\n\
         \t<dict>\n\
         \t\t<key>SuccessfulExit</key>\n\
         \t\t<false/>\n\
         \t</dict>\n\
         </dict>\n\
         </plist>\n"
    )
}

#[cfg(target_os = "linux")]
fn unit_path() -> Option<PathBuf> {
    dirs::config_dir().map(|c| c.join("systemd/user/ryuzi.service"))
}

#[cfg(target_os = "macos")]
fn agent_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join("Library/LaunchAgents/dev.ryuzi.daemon.plist"))
}

/// Run a manager command, reporting success/failure to `out` without
/// failing the whole install — the unit file on disk is the deliverable;
/// the activation commands are convenience.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
fn try_run(out: &mut Box<dyn FnMut(&str)>, program: &str, args: &[&str]) {
    match Command::new(program).args(args).status() {
        Ok(s) if s.success() => (out)(&format!("ran: {program} {}", args.join(" "))),
        Ok(s) => (out)(&format!(
            "warning: `{program} {}` exited with {s} — run it manually",
            args.join(" ")
        )),
        Err(e) => (out)(&format!(
            "warning: could not run `{program} {}`: {e} — run it manually",
            args.join(" ")
        )),
    }
}

#[cfg(target_os = "linux")]
fn install(deps: &mut Deps) -> u8 {
    let Ok(exe) = std::env::current_exe() else {
        (deps.err)("service: cannot resolve the current executable path");
        return 1;
    };
    let Some(path) = unit_path() else {
        (deps.err)("service: cannot resolve the user config directory");
        return 1;
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            (deps.err)(&format!("service: {e}"));
            return 1;
        }
    }
    if let Err(e) = std::fs::write(&path, systemd_unit(&exe.to_string_lossy())) {
        (deps.err)(&format!("service: {e}"));
        return 1;
    }
    (deps.out)(&format!("wrote {}", path.display()));
    try_run(&mut deps.out, "systemctl", &["--user", "daemon-reload"]);
    try_run(
        &mut deps.out,
        "systemctl",
        &["--user", "enable", "--now", "ryuzi"],
    );
    0
}

#[cfg(target_os = "linux")]
fn uninstall(deps: &mut Deps) -> u8 {
    try_run(
        &mut deps.out,
        "systemctl",
        &["--user", "disable", "--now", "ryuzi"],
    );
    match unit_path() {
        Some(path) if path.exists() => match std::fs::remove_file(&path) {
            Ok(()) => {
                (deps.out)(&format!("removed {}", path.display()));
                0
            }
            Err(e) => {
                (deps.err)(&format!("service: {e}"));
                1
            }
        },
        _ => {
            (deps.out)("service: no unit file installed");
            0
        }
    }
}

#[cfg(target_os = "linux")]
fn status(deps: &mut Deps) -> u8 {
    try_run(
        &mut deps.out,
        "systemctl",
        &["--user", "status", "ryuzi", "--no-pager"],
    );
    0
}

#[cfg(target_os = "macos")]
fn install(deps: &mut Deps) -> u8 {
    let Ok(exe) = std::env::current_exe() else {
        (deps.err)("service: cannot resolve the current executable path");
        return 1;
    };
    let Some(path) = agent_path() else {
        (deps.err)("service: cannot resolve the home directory");
        return 1;
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            (deps.err)(&format!("service: {e}"));
            return 1;
        }
    }
    if let Err(e) = std::fs::write(&path, launchd_plist(&exe.to_string_lossy())) {
        (deps.err)(&format!("service: {e}"));
        return 1;
    }
    (deps.out)(&format!("wrote {}", path.display()));
    try_run(
        &mut deps.out,
        "launchctl",
        &["load", "-w", &path.to_string_lossy()],
    );
    0
}

#[cfg(target_os = "macos")]
fn uninstall(deps: &mut Deps) -> u8 {
    match agent_path() {
        Some(path) if path.exists() => {
            try_run(
                &mut deps.out,
                "launchctl",
                &["unload", "-w", &path.to_string_lossy()],
            );
            match std::fs::remove_file(&path) {
                Ok(()) => {
                    (deps.out)(&format!("removed {}", path.display()));
                    0
                }
                Err(e) => {
                    (deps.err)(&format!("service: {e}"));
                    1
                }
            }
        }
        _ => {
            (deps.out)("service: no launch agent installed");
            0
        }
    }
}

#[cfg(target_os = "macos")]
fn status(deps: &mut Deps) -> u8 {
    try_run(&mut deps.out, "launchctl", &["list", "dev.ryuzi.daemon"]);
    0
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn install(deps: &mut Deps) -> u8 {
    unsupported(deps)
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn uninstall(deps: &mut Deps) -> u8 {
    unsupported(deps)
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn status(deps: &mut Deps) -> u8 {
    unsupported(deps)
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn unsupported(deps: &mut Deps) -> u8 {
    (deps.err)(
        "service: not supported on this platform yet — run `ryuzi start` under your service manager",
    );
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systemd_unit_execs_the_binary_with_start_and_restarts_on_failure() {
        let unit = systemd_unit("/home/me/.local/bin/ryuzi");
        assert!(unit.contains("ExecStart=/home/me/.local/bin/ryuzi start"));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("WantedBy=default.target"));
    }

    #[test]
    fn launchd_plist_carries_label_program_args_and_keepalive() {
        let plist = launchd_plist("/usr/local/bin/ryuzi");
        assert!(plist.contains("<string>dev.ryuzi.daemon</string>"));
        assert!(plist.contains("<string>/usr/local/bin/ryuzi</string>"));
        assert!(plist.contains("<string>start</string>"));
        assert!(plist.contains("<key>KeepAlive</key>"));
    }

    #[test]
    fn service_without_a_subcommand_prints_usage_and_fails() {
        let errs = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = errs.clone();
        let mut deps = Deps {
            db_path: std::path::PathBuf::from("unused.sqlite"),
            out: Box::new(|_| {}),
            err: Box::new(move |s| sink.borrow_mut().push(s.to_string())),
            prompt: Box::new(|_| String::new()),
            detect_git: || crate::detect::Detected {
                found: false,
                version: None,
            },
        };
        assert_eq!(cmd_service(&[], &mut deps), 1);
        assert!(errs.borrow()[0].contains("usage: ryuzi service"));
    }
}
