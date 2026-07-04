//! The `update.json` file the old daemon (applier) and the canary use to
//! hand a deployment over. Wire format is frozen; see the wire_format test.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Handoff {
    pub phase: HandoffPhase,
    pub pid: i32,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HandoffPhase {
    Probing,
    Healthy,
    Failed,
    Promote,
    Promoted,
}

pub fn handoff_path(dir: &Path) -> PathBuf {
    dir.join("update.json")
}

pub fn read_handoff(dir: &Path) -> Option<Handoff> {
    let content = std::fs::read_to_string(handoff_path(dir)).ok()?;
    serde_json::from_str(&content).ok()
}

pub fn write_handoff(dir: &Path, h: &Handoff) -> std::io::Result<()> {
    let json = serde_json::to_string(h)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(handoff_path(dir), json)
}

pub fn clear_handoff(dir: &Path) {
    let _ = std::fs::remove_file(handoff_path(dir));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_roundtrip_clear_and_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_handoff(dir.path()), None);
        let h = Handoff {
            phase: HandoffPhase::Probing,
            pid: 4242,
            version: "0.3.0".into(),
            detail: None,
        };
        write_handoff(dir.path(), &h).unwrap();
        assert_eq!(read_handoff(dir.path()), Some(h));
        let f = Handoff {
            phase: HandoffPhase::Failed,
            pid: 4242,
            version: "0.3.0".into(),
            detail: Some("db open failed".into()),
        };
        write_handoff(dir.path(), &f).unwrap();
        assert_eq!(
            read_handoff(dir.path()).unwrap().detail.as_deref(),
            Some("db open failed")
        );
        clear_handoff(dir.path());
        assert_eq!(read_handoff(dir.path()), None);
        clear_handoff(dir.path()); // second clear is a no-op
        std::fs::write(handoff_path(dir.path()), "{not json").unwrap();
        assert_eq!(read_handoff(dir.path()), None);
    }

    #[test]
    fn wire_format_matches_ts_exactly() {
        let dir = tempfile::tempdir().unwrap();
        write_handoff(
            dir.path(),
            &Handoff {
                phase: HandoffPhase::Promote,
                pid: 7,
                version: "0.3.0".into(),
                detail: None,
            },
        )
        .unwrap();
        let raw = std::fs::read_to_string(handoff_path(dir.path())).unwrap();
        assert_eq!(raw, r#"{"phase":"promote","pid":7,"version":"0.3.0"}"#);
    }
}
