//! Attachment handling: the per-session download directory and folding a
//! downloaded-attachment manifest into the outgoing prompt.

use super::ControlPlane;
use crate::attachments::{build_manifest, materialize_attachments, MaterializeOpts};
use crate::domain::AttachmentRef;
use crate::settings::{expand_home, SettingsStore};
use std::path::PathBuf;
use std::sync::Arc;

impl ControlPlane {
    /// `{expand_home(workdir_root)}/.harness-attachments/{session_pk}` — the
    /// dest dir attachments are downloaded into (`with_attachments`) and torn
    /// down from (`end_session`). Reads `workdir_root` fresh each call: it's
    /// a rarely-changed setting, and this avoids caching it on `ControlPlane`.
    pub(super) async fn attachment_dest_dir(&self, session_pk: &str) -> PathBuf {
        let settings = SettingsStore::new(Arc::clone(&self.store));
        let root_raw = settings
            .get("workdir_root")
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        expand_home(&root_raw)
            .join(".harness-attachments")
            .join(session_pk)
    }

    /// Materializes any Discord-supplied attachments into
    /// `.harness-attachments/{session_pk}` and folds the resulting manifest
    /// into the prompt.
    pub(super) async fn with_attachments(
        &self,
        session_pk: &str,
        prompt: &str,
        attachments: &[AttachmentRef],
    ) -> String {
        if attachments.is_empty() {
            return prompt.to_string();
        }
        let settings = SettingsStore::new(Arc::clone(&self.store));
        let max_count: i64 = settings
            .get("attachment_max_count")
            .await
            .ok()
            .flatten()
            .and_then(|s| s.parse().ok())
            .unwrap_or(10);
        if max_count <= 0 {
            // feature disabled
            return if prompt.is_empty() {
                "User sent attachments, but attachment support is disabled.".to_string()
            } else {
                prompt.to_string()
            };
        }

        let dest_dir = self.attachment_dest_dir(session_pk).await;
        let max_bytes: u64 = settings
            .get("attachment_max_bytes")
            .await
            .ok()
            .flatten()
            .and_then(|s| s.parse().ok())
            .unwrap_or(26_214_400);
        let allowed_ext_raw = settings.get("attachment_allowed_ext").await.ok().flatten();
        let allowed_hosts_raw = settings
            .get("attachment_allowed_hosts")
            .await
            .ok()
            .flatten();

        let opts = MaterializeOpts {
            dest_dir,
            max_bytes,
            max_count: max_count as u32,
            allowed_ext: crate::attachments::parse_allowed_ext(allowed_ext_raw.as_deref()),
            allowed_hosts: crate::attachments::parse_allowed_hosts(allowed_hosts_raw.as_deref()),
        };

        let result =
            match materialize_attachments(attachments, &opts, Arc::clone(&self.attachment_fetcher))
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    return if !prompt.is_empty() {
                        format!("{prompt}\n\n⚠️ Could not process attachments: {e}")
                    } else {
                        format!("User sent attachments, but they could not be processed: {e}")
                    };
                }
            };

        let manifest = build_manifest(&result);
        if manifest.is_empty() {
            return prompt.to_string();
        }
        if prompt.is_empty() {
            return if !result.saved.is_empty() {
                format!("User sent attachments with no message text.\n\n{manifest}")
            } else {
                format!("User sent attachments but none could be processed:\n{manifest}")
            };
        }
        format!("{prompt}\n\n{manifest}")
    }
}
