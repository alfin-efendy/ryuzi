//! Daemon-hosted durable learning queue worker.

use std::sync::Arc;
use std::time::Duration;

use crate::agents::learning_queue::LearningQueue;
use crate::paths;

const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Process at most one event per agent, preserving strict per-agent sequence.
pub async fn tick(queue: &Arc<LearningQueue>, worker_id: &str) {
    if let Err(error) = queue.reclaim_stale(paths::now_ms()).await {
        tracing::warn!("learning queue reclaim failed: {error}");
        return;
    }
    for agent_id in queue.pending_agents().await.unwrap_or_default() {
        let Ok(Some(event)) = queue.claim_next(&agent_id, worker_id).await else {
            continue;
        };
        match queue.apply_claimed(&event).await {
            Ok(()) => {
                if let Err(error) = queue.mark_delivered(&event.event_id).await {
                    tracing::warn!(event_id = %event.event_id, "learning queue ack failed: {error}");
                }
            }
            Err(error) => {
                if let Err(release_error) = queue.release(&event.event_id, &error.to_string()).await
                {
                    tracing::warn!(event_id = %event.event_id, "learning queue release failed: {release_error}");
                }
            }
        }
    }
}

pub async fn run_loop(queue: Arc<LearningQueue>) {
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        tick(&queue, "daemon-learning").await;
    }
}

pub fn spawn_runner(queue: Arc<LearningQueue>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run_loop(queue))
}
