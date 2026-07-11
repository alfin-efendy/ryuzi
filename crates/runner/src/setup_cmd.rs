use std::sync::Arc;

use ryuzi_core::settings::{find_field, SettingsStore};

use crate::dispatch::Deps;

pub fn cmd_setup(deps: &mut Deps) -> u8 {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(setup_inner(deps))
}

async fn setup_inner(deps: &mut Deps) -> u8 {
    // Same reasoning as config_cmd: plugin fields must be registered so
    // `missing_required`/`is_secret` see plugin-declared settings.
    ryuzi_core::plugins::register_builtin_plugin_fields();
    let store = match crate::db::open_store(deps).await {
        Ok(s) => Arc::new(s),
        Err(e) => {
            (deps.err)(&format!("✗ {e}"));
            return 1;
        }
    };
    let settings = SettingsStore::new(store);
    let missing = match settings.missing_required().await {
        Ok(m) => m,
        Err(e) => {
            (deps.err)(&format!("✗ {e}"));
            return 1;
        }
    };
    if missing.is_empty() {
        (deps.out)("setup: all required settings are present — run `ryuzi start`");
        return 0;
    }
    for key in missing {
        let hint = find_field(key)
            .map(|f| format!("{} — {}", f.label, f.help))
            .unwrap_or_else(|| key.to_string());
        let raw = (deps.prompt)(&format!("{hint}\n{key} = "));
        let value = raw.trim();
        if value.is_empty() {
            (deps.out)(&format!("skipped {key}"));
            continue;
        }
        if let Err(e) = settings.set(key, value).await {
            (deps.err)(&format!("✗ {key}: {e}"));
            return 1;
        }
        (deps.out)(&format!("set {key}"));
    }
    match settings.missing_required().await {
        Ok(still) if still.is_empty() => {
            (deps.out)("setup: complete — run `ryuzi start`");
            0
        }
        Ok(still) => {
            (deps.out)(&format!("setup: still missing: {}", still.join(", ")));
            1
        }
        Err(e) => {
            (deps.err)(&format!("✗ {e}"));
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn deps_with_prompts(
        db: &std::path::Path,
        answers: Vec<&'static str>,
    ) -> (Deps, Rc<RefCell<Vec<String>>>) {
        let out = Rc::new(RefCell::new(Vec::new()));
        let sink = out.clone();
        let answers = Rc::new(RefCell::new(answers));
        let deps = Deps {
            db_path: db.to_path_buf(),
            out: Box::new(move |s| sink.borrow_mut().push(s.to_string())),
            err: Box::new(|_| {}),
            prompt: Box::new(move |_q| {
                let mut a = answers.borrow_mut();
                if a.is_empty() {
                    String::new()
                } else {
                    a.remove(0).to_string()
                }
            }),
            detect_git: || crate::detect::Detected {
                found: true,
                version: None,
            },
        };
        (deps, out)
    }

    #[test]
    fn setup_prompts_for_each_missing_required_and_exits_zero_when_done() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("d.sqlite");
        // Fresh DB: default enabled gateway config requires workdir_root +
        // discord fields (see doctor.rs tests). Answer them all.
        let (mut deps, out) = deps_with_prompts(&db, vec!["/repos", "tok", "app", "guild"]);
        let code = cmd_setup(&mut deps);
        assert_eq!(code, 0, "all answered → success: {:?}", out.borrow());
        assert!(out
            .borrow()
            .iter()
            .any(|l| l == "setup: complete — run `ryuzi start`"));
    }

    #[test]
    fn setup_reports_still_missing_and_exits_one_when_answers_are_blank() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("d.sqlite");
        let (mut deps, out) = deps_with_prompts(&db, vec![]);
        let code = cmd_setup(&mut deps);
        assert_eq!(code, 1);
        assert!(out
            .borrow()
            .iter()
            .any(|l| l.starts_with("setup: still missing:")));
    }

    #[test]
    fn setup_is_a_noop_when_nothing_is_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("d.sqlite");
        {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = ryuzi_core::Store::open(&db).await.unwrap();
                let settings = SettingsStore::new(Arc::new(store));
                for (k, v) in [
                    ("workdir_root", "/repos"),
                    ("discord.token", "t"),
                    ("discord.app_id", "a"),
                    ("discord.guild_id", "g"),
                ] {
                    settings.set(k, v).await.unwrap();
                }
            });
        }
        let (mut deps, out) = deps_with_prompts(&db, vec![]);
        let code = cmd_setup(&mut deps);
        assert_eq!(code, 0);
        assert!(out
            .borrow()
            .iter()
            .any(|l| l.contains("all required settings are present")));
    }
}
