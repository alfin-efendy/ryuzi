use crate::dispatch::Deps;
use crate::paint::{paint, Tone};

pub fn cmd_doctor(deps: &mut Deps) -> u8 {
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
    // settings check arrives with the SettingsStore port (Plan 4B); acp line with Task 6.
    let ok = git.found && claude.found;
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

    fn fake_deps(
        git: Detected,
        claude: Detected,
    ) -> (
        crate::dispatch::Deps,
        std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    ) {
        let out = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = out.clone();
        let deps = crate::dispatch::Deps {
            db_path: std::env::temp_dir().join("unused.sqlite"),
            out: Box::new(move |s| sink.borrow_mut().push(s.to_string())),
            err: Box::new(|_| {}),
            prompt: Box::new(|_| String::new()),
            detect_git: if git.found {
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
            detect_claude: if claude.found {
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
        };
        (deps, out)
    }

    #[test]
    fn doctor_pass_when_git_and_claude_found() {
        let (mut deps, out) = fake_deps(
            Detected {
                found: true,
                version: None,
            },
            Detected {
                found: true,
                version: None,
            },
        );
        let code = cmd_doctor(&mut deps);
        let lines = out.borrow();
        assert_eq!(lines[0], "git:    OK 2.45.0");
        assert_eq!(lines[1], "claude: OK 2.1.89");
        assert_eq!(lines[2], "auth:   unknown (relies on host login)");
        assert_eq!(lines[3], "doctor: PASS");
        assert_eq!(code, 0);
        assert!(lines.iter().all(|l| !l.contains('\u{1b}'))); // no ANSI when not a TTY
    }

    #[test]
    fn doctor_fail_when_claude_missing() {
        let (mut deps, out) = fake_deps(
            Detected {
                found: true,
                version: None,
            },
            Detected {
                found: false,
                version: None,
            },
        );
        let code = cmd_doctor(&mut deps);
        let lines = out.borrow();
        assert_eq!(lines[1], "claude: NOT FOUND");
        assert_eq!(lines[2], "auth:   n/a");
        assert_eq!(lines[3], "doctor: FAIL");
        assert_eq!(code, 1);
    }
}
