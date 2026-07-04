//! Detects how this binary was installed. Only the install.sh path
//! (`{home}/.local/bin/ryuzi`) is self-applicable — every other method is
//! owned by a package manager (or is dev/docker) and must be notify-only so
//! the daemon never clobbers a package manager's binary.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InstallMethod {
    InstallSh,
    Npm,
    Brew,
    Scoop,
    Docker,
    Dev,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InstallInfo {
    pub method: InstallMethod,
    pub self_applicable: bool,
}

pub fn detect_install_method(
    exec_path: &str,
    compiled: bool,
    home: Option<&str>,
    docker_env: bool,
) -> InstallInfo {
    let no = |method| InstallInfo {
        method,
        self_applicable: false,
    };
    if !compiled {
        return no(InstallMethod::Dev);
    }
    if docker_env {
        return no(InstallMethod::Docker);
    }
    let lower = exec_path.to_lowercase();
    if let Some(home) = home {
        if exec_path == format!("{home}/.local/bin/ryuzi") {
            return InstallInfo {
                method: InstallMethod::InstallSh,
                self_applicable: true,
            };
        }
    }
    if lower.contains("/cellar/")
        || lower.starts_with("/opt/homebrew/")
        || lower.contains("/homebrew/")
    {
        return no(InstallMethod::Brew);
    }
    if lower.contains("\\scoop\\") || lower.contains("/scoop/") {
        return no(InstallMethod::Scoop);
    }
    if lower.contains("/node_modules/") || lower.contains("\\node_modules\\") {
        return no(InstallMethod::Npm);
    }
    no(InstallMethod::Unknown)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(p: &str) -> InstallInfo {
        detect_install_method(p, true, Some("/home/me"), false)
    }

    #[test]
    fn only_installsh_under_home_local_bin_is_self_applicable() {
        let i = m("/home/me/.local/bin/ryuzi");
        assert_eq!(i.method, InstallMethod::InstallSh);
        assert!(i.self_applicable);
        // a DIFFERENT home's path is not
        assert_eq!(
            m("/home/other/.local/bin/ryuzi").method,
            InstallMethod::Unknown
        );
    }

    #[test]
    fn package_managers_docker_and_dev_are_notify_only() {
        assert_eq!(m("/opt/homebrew/bin/ryuzi").method, InstallMethod::Brew);
        assert_eq!(
            m("/usr/local/Cellar/ryuzi/0.2.0/bin/ryuzi").method,
            InstallMethod::Brew
        );
        assert_eq!(
            m(r"C:\Users\me\scoop\apps\ryuzi\current\ryuzi.exe").method,
            InstallMethod::Scoop
        );
        assert_eq!(
            m("/usr/local/lib/node_modules/ryuzi/bin/ryuzi").method,
            InstallMethod::Npm
        );
        assert_eq!(m("/weird/place/ryuzi").method, InstallMethod::Unknown);
        let dev =
            detect_install_method("/home/me/.local/bin/ryuzi", false, Some("/home/me"), false);
        assert_eq!(dev.method, InstallMethod::Dev);
        let docker =
            detect_install_method("/home/me/.local/bin/ryuzi", true, Some("/home/me"), true);
        assert_eq!(docker.method, InstallMethod::Docker);
        for i in [
            m("/opt/homebrew/bin/ryuzi"),
            m("/usr/local/lib/node_modules/ryuzi/bin/ryuzi"),
            dev,
            docker,
        ] {
            assert!(!i.self_applicable);
        }
    }
}
