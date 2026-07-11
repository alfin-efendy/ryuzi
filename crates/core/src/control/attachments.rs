//! Attachment handling: the per-session download directory and folding a
//! downloaded-attachment manifest into the outgoing prompt.

use super::ControlPlane;
use crate::attachments::{build_manifest, materialize_attachments, MaterializeOpts};
use crate::domain::AttachmentRef;
use crate::settings::{expand_home, SettingsStore};
use std::path::PathBuf;
use std::sync::Arc;

/// What a turn's attachments contribute to the outgoing prompt: the
/// (possibly manifest-decorated) agent text, Anthropic image blocks for
/// eligible images, and display metadata for the user transcript row.
pub(super) struct PreparedPrompt {
    pub agent: String,
    pub image_blocks: Vec<serde_json::Value>,
    pub attachments_meta: Vec<serde_json::Value>,
}

impl PreparedPrompt {
    fn text_only(agent: String) -> Self {
        PreparedPrompt {
            agent,
            image_blocks: Vec::new(),
            attachments_meta: Vec::new(),
        }
    }
}

impl ControlPlane {
    /// `{expand_home(workdir_root)}/.harness-attachments` — the root all
    /// per-session attachment dirs (and the paste staging area) live under.
    /// Public: the cockpit shell scopes the asset protocol to it and stages
    /// pasted files into `{root}/staging/`.
    pub async fn attachments_root(&self) -> PathBuf {
        let settings = SettingsStore::new(Arc::clone(&self.store));
        let root_raw = settings
            .get("workdir_root")
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        expand_home(&root_raw).join(".harness-attachments")
    }

    /// `{attachments_root()}/{session_pk}` — the dest dir attachments are
    /// downloaded into (`prepare_attachments`) and torn down from
    /// (`end_session`).
    pub(super) async fn attachment_dest_dir(&self, session_pk: &str) -> PathBuf {
        self.attachments_root().await.join(session_pk)
    }

    /// Materializes any Discord-supplied attachments into
    /// `.harness-attachments/{session_pk}` and folds the resulting manifest
    /// into the prompt, alongside any Anthropic vision blocks and display
    /// metadata the eligible saved images contribute.
    pub(super) async fn prepare_attachments(
        &self,
        session_pk: &str,
        prompt: &str,
        attachments: &[AttachmentRef],
    ) -> PreparedPrompt {
        if attachments.is_empty() {
            return PreparedPrompt::text_only(prompt.to_string());
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
            return PreparedPrompt::text_only(if prompt.is_empty() {
                "User sent attachments, but attachment support is disabled.".to_string()
            } else {
                prompt.to_string()
            });
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
                    return PreparedPrompt::text_only(if !prompt.is_empty() {
                        format!("{prompt}\n\n⚠️ Could not process attachments: {e}")
                    } else {
                        format!("User sent attachments, but they could not be processed: {e}")
                    });
                }
            };

        let (image_blocks, inlined) = crate::attachments::image_blocks_for(&result.saved).await;
        let attachments_meta = crate::attachments::attachment_display_meta(
            &result.saved,
            &self.attachments_root().await,
        );
        let manifest = build_manifest(&result, &inlined);
        let agent = if manifest.is_empty() {
            prompt.to_string()
        } else if prompt.is_empty() {
            if !result.saved.is_empty() {
                format!("User sent attachments with no message text.\n\n{manifest}")
            } else {
                format!("User sent attachments but none could be processed:\n{manifest}")
            }
        } else {
            format!("{prompt}\n\n{manifest}")
        };
        PreparedPrompt {
            agent,
            image_blocks,
            attachments_meta,
        }
    }
}
