use crate::dispatch::Deps;
use crate::paint::{paint, Tone};

pub fn cmd_doctor(deps: &mut Deps) -> u8 {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(doctor_inner(deps))
}

async fn doctor_inner(deps: &mut Deps) -> u8 {
    let git = (deps.detect_git)();

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
    let ok = git.found && missing.is_empty();
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

    fn fake_deps_with_db(
        db: &std::path::Path,
        git_found: bool,
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
        let (mut deps, out) = fake_deps_with_db(&db, /* git found */ true);
        let code = cmd_doctor(&mut deps);
        let lines = out.borrow();
        assert_eq!(lines[0], "git:    OK 2.45.0");
        assert_eq!(lines[1], "settings: OK");
        assert_eq!(lines[2], "doctor: PASS");
        assert_eq!(lines.len(), 3, "doctor prints exactly 3 lines");
        assert_eq!(code, 0);
        assert!(lines.iter().all(|l| !l.contains('\u{1b}'))); // no ANSI when not a TTY
    }

    #[test]
    fn doctor_fresh_db_reports_missing_and_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("d.sqlite");
        let (mut deps, out) = fake_deps_with_db(&db, true);
        let code = cmd_doctor(&mut deps);
        let lines = out.borrow();
        assert_eq!(
            lines[1],
            "settings: missing workdir_root, discord.token, discord.app_id, discord.guild_id"
        );
        assert_eq!(lines[2], "doctor: FAIL");
        assert_eq!(code, 1);
    }

    #[test]
    fn doctor_fail_when_git_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("d.sqlite");
        seed_required(&db);
        let (mut deps, out) = fake_deps_with_db(&db, false);
        let code = cmd_doctor(&mut deps);
        let lines = out.borrow();
        assert_eq!(lines[0], "git:    NOT FOUND");
        assert_eq!(lines[2], "doctor: FAIL");
        assert_eq!(code, 1);
    }
}
