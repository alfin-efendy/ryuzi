use crate::dispatch::Deps;
use crate::paint::{paint, Tone};
use ryuzi_core::sidecar::SidecarStatus;

pub fn cmd_doctor(deps: &mut Deps) -> u8 {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(doctor_inner(deps))
}

async fn doctor_inner(deps: &mut Deps) -> u8 {
    let git = (deps.detect_git)();
    let claude = (deps.detect_claude)();

    let render = |found: bool, version: &Option<String>| -> String {
        if found {
            format!(
                "{} {}",
                paint("OK", Tone::Ok, false),
                version.clone().unwrap_or_default()
            )
        } else {
            paint("NOT FOUND", Tone::Bad, false)
        }
    };
    (deps.out)(&format!(
        "git:    {}",
        render(git.found, &git.version).trim_end()
    ));
    (deps.out)(&format!(
        "claude: {}",
        render(claude.found, &claude.version).trim_end()
    ));
    (deps.out)(&format!(
        "auth:   {}",
        if claude.found {
            "unknown (relies on host login)"
        } else {
            "n/a"
        }
    ));
    let missing: Vec<&'static str> = match crate::db::open_store(deps).await {
        Ok(store) => {
            let settings = ryuzi_core::settings::SettingsStore::new(std::sync::Arc::new(store));
            match settings.missing_required().await {
                Ok(m) => m,
                Err(e) => {
                    (deps.err)(&format!("✗ {e}"));
                    return 1;
                }
            }
        }
        Err(e) => {
            (deps.err)(&format!("✗ {e}"));
            return 1;
        }
    };
    (deps.out)(&if missing.is_empty() {
        format!("settings: {}", paint("OK", Tone::Ok, false))
    } else {
        format!(
            "settings: {}",
            paint(
                &format!("missing {}", missing.join(", ")),
                Tone::Warn,
                false
            )
        )
    });
    let sidecar = (deps.sidecar_status)();
    let acp_line = match sidecar {
        SidecarStatus::Override => paint("OK (override)", Tone::Ok, false),
        SidecarStatus::CachedBundle => paint("OK (bun)", Tone::Ok, false),
        SidecarStatus::CachedStandalone => paint("OK (standalone)", Tone::Ok, false),
        SidecarStatus::NeedsDownloadBundle => paint(
            "not cached (bun detected - JS bundle downloads on first run)",
            Tone::Warn,
            false,
        ),
        SidecarStatus::NeedsDownloadStandalone => paint(
            "not cached (no bun - standalone binary downloads on first run)",
            Tone::Warn,
            false,
        ),
    };
    (deps.out)(&format!("acp:    {acp_line}"));
    // The acp line never affects the exit code.
    let ok = git.found && claude.found && missing.is_empty();
    (deps.out)(&format!(
        "doctor: {}",
        if ok {
            paint("PASS", Tone::Ok, true)
        } else {
            paint("FAIL", Tone::Bad, false)
        }
    ));
    if ok {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::Detected;
    use ryuzi_core::sidecar::SidecarStatus;

    fn fake_deps_with_db(
        db: &std::path::Path,
        git_found: bool,
        claude_found: bool,
    ) -> (
        crate::dispatch::Deps,
        std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    ) {
        fake_deps_with_sidecar_inner(db, git_found, claude_found, SidecarStatus::Override)
    }

    fn fake_deps_with_sidecar_inner(
        db: &std::path::Path,
        git_found: bool,
        claude_found: bool,
        sidecar: SidecarStatus,
    ) -> (
        crate::dispatch::Deps,
        std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    ) {
        let out = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = out.clone();
        let deps = crate::dispatch::Deps {
            db_path: db.to_path_buf(),
            out: Box::new(move |s| sink.borrow_mut().push(s.to_string())),
            err: Box::new(|_| {}),
            prompt: Box::new(|_| String::new()),
            detect_git: if git_found {
                || Detected {
                    found: true,
                    version: Some("2.45.0".into()),
                }
            } else {
                || Detected {
                    found: false,
                    version: None,
                }
            },
            detect_claude: if claude_found {
                || Detected {
                    found: true,
                    version: Some("2.1.89".into()),
                }
            } else {
                || Detected {
                    found: false,
                    version: None,
                }
            },
            sidecar_status: Box::new(move || sidecar.clone()),
            build_registries: Box::new(|| Ok(ryuzi_core::Registries::new())),
        };
        (deps, out)
    }

    #[tokio::main(flavor = "current_thread")]
    async fn seed_required(db: &std::path::Path) {
        let store = std::sync::Arc::new(ryuzi_core::Store::open(db).await.unwrap());
        let settings = ryuzi_core::settings::SettingsStore::new(store);
        for (k, v) in [
            ("workdir_root", "/repos"),
            ("discord.token", "t"),
            ("discord.app_id", "a"),
            ("discord.guild_id", "g"),
        ] {
            settings.set(k, v).await.unwrap();
        }
    }

    #[test]
    fn doctor_pass_when_tools_present_and_settings_seeded() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("d.sqlite");
        seed_required(&db);
        let (mut deps, out) =
            fake_deps_with_db(&db, /* git found */ true, /* claude found */ true);
        let code = cmd_doctor(&mut deps);
        let lines = out.borrow();
        assert_eq!(lines[0], "git:    OK 2.45.0");
        assert_eq!(lines[1], "claude: OK 2.1.89");
        assert_eq!(lines[2], "auth:   unknown (relies on host login)");
        assert_eq!(lines[3], "settings: OK");
        assert_eq!(lines[4], "acp:    OK (override)");
        assert_eq!(lines[5], "doctor: PASS");
        assert_eq!(code, 0);
        assert!(lines.iter().all(|l| !l.contains('\u{1b}'))); // no ANSI when not a TTY
    }

    #[test]
    fn doctor_fresh_db_reports_missing_and_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("d.sqlite");
        let (mut deps, out) = fake_deps_with_db(&db, true, true);
        let code = cmd_doctor(&mut deps);
        let lines = out.borrow();
        assert_eq!(
            lines[3],
            "settings: missing workdir_root, discord.token, discord.app_id, discord.guild_id"
        );
        assert_eq!(lines[5], "doctor: FAIL");
        assert_eq!(code, 1);
    }

    #[test]
    fn doctor_fail_when_claude_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("d.sqlite");
        let (mut deps, out) = fake_deps_with_db(&db, true, false);
        let code = cmd_doctor(&mut deps);
        let lines = out.borrow();
        assert_eq!(lines[1], "claude: NOT FOUND");
        assert_eq!(lines[2], "auth:   n/a");
        assert_eq!(lines[5], "doctor: FAIL");
        assert_eq!(code, 1);
    }

    #[test]
    fn doctor_reports_acp_mode() {
        use ryuzi_core::sidecar::SidecarStatus;
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("d.sqlite");
        for (status, line) in [
            (SidecarStatus::Override, "acp:    OK (override)"),
            (SidecarStatus::CachedBundle, "acp:    OK (bun)"),
            (SidecarStatus::CachedStandalone, "acp:    OK (standalone)"),
            (
                SidecarStatus::NeedsDownloadBundle,
                "acp:    not cached (bun detected - JS bundle downloads on first run)",
            ),
            (
                SidecarStatus::NeedsDownloadStandalone,
                "acp:    not cached (no bun - standalone binary downloads on first run)",
            ),
        ] {
            let (mut deps, out) = fake_deps_with_sidecar_inner(&db, true, true, status);
            cmd_doctor(&mut deps);
            assert_eq!(out.borrow()[4], line); // between settings: and doctor:
        }
    }
}
