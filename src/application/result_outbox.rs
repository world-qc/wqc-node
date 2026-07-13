use std::sync::Arc;
use std::time::Duration;

use crate::application::state::AppState;
use crate::domain::models::{ComputeTask, TaskResultPayload};
use crate::transport::p2p::result_delivery::{build_result_wire_body, send_result_wire};

const DEFAULT_RETRY_INTERVAL_SECS: u64 = 5;

/// Persists the P2P wire payload, updates task status, and attempts delivery.
pub async fn deliver_result(
    state: Arc<AppState>,
    task: &ComputeTask,
    payload: &TaskResultPayload,
) -> anyhow::Result<()> {
    let sub_task_id = task.request.task_id.clone();
    let orchestrator_pubkey = task.orchestrator_pubkey.clone();

    let wire_body = build_result_wire_body(task, payload, &state.config.peer_id)?;

    state
        .storage
        .upsert_pending_result(&orchestrator_pubkey, &sub_task_id, &wire_body)?;

    let status = if payload.status == "error" {
        "failed"
    } else {
        "completed"
    };
    if let Err(e) = state
        .storage
        .update_status(&orchestrator_pubkey, &sub_task_id, status)
    {
        tracing::error!(
            "Storage status update failed for sub_task_id={} owned by {}: {}",
            sub_task_id,
            orchestrator_pubkey,
            e
        );
    }

    try_deliver_pending(state, &orchestrator_pubkey, &sub_task_id, &wire_body).await
}

async fn try_deliver_pending(
    state: Arc<AppState>,
    orchestrator_pubkey: &str,
    sub_task_id: &str,
    wire_body: &[u8],
) -> anyhow::Result<()> {
    match send_result_wire(state.clone(), wire_body).await {
        Ok(()) => {
            state
                .storage
                .delete_pending_result(orchestrator_pubkey, sub_task_id)?;
            crate::infra::metrics::record_result_delivery("initial", "ok");
            tracing::info!(
                "[Result Outbox] Delivered sub_task_id={} to orchestrator",
                sub_task_id
            );
            Ok(())
        }
        Err(e) => {
            crate::infra::metrics::record_result_delivery("initial", "error");
            tracing::warn!(
                "[Result Outbox] Delivery failed for sub_task_id={} (queued for retry): {}",
                sub_task_id,
                e
            );
            Err(e)
        }
    }
}

pub fn spawn_retry_loop(state: Arc<AppState>) {
    let interval_secs = std::env::var("WQC_RESULT_RETRY_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_RETRY_INTERVAL_SECS);

    tokio::spawn(async move {
        let interval = Duration::from_secs(interval_secs);
        tracing::info!(
            "[Result Outbox] Retry loop started (interval={}s)",
            interval_secs
        );

        loop {
            tokio::time::sleep(interval).await;

            let pending = match state.storage.list_pending_results() {
                Ok(entries) => entries,
                Err(e) => {
                    tracing::error!("[Result Outbox] Failed to list pending results: {}", e);
                    continue;
                }
            };

            if pending.is_empty() {
                continue;
            }

            tracing::debug!(
                "[Result Outbox] Retrying {} pending result(s)",
                pending.len()
            );

            for entry in pending {
                match send_result_wire(state.clone(), &entry.wire_body).await {
                    Ok(()) => {
                        crate::infra::metrics::record_result_delivery("retry", "ok");
                        if let Err(e) = state
                            .storage
                            .delete_pending_result(&entry.orchestrator_pubkey, &entry.sub_task_id)
                        {
                            tracing::error!(
                                "[Result Outbox] Failed to delete delivered sub_task_id={}: {}",
                                entry.sub_task_id,
                                e
                            );
                        } else {
                            tracing::info!(
                                "[Result Outbox] Retry delivered sub_task_id={} after {} attempt(s)",
                                entry.sub_task_id,
                                entry.attempts
                            );
                        }
                    }
                    Err(e) => {
                        crate::infra::metrics::record_result_delivery("retry", "error");
                        if let Err(inc_err) =
                            state.storage.increment_pending_result_attempts(entry.id)
                        {
                            tracing::error!(
                                "[Result Outbox] Failed to increment attempts for sub_task_id={}: {}",
                                entry.sub_task_id,
                                inc_err
                            );
                        }
                        tracing::debug!(
                            "[Result Outbox] Retry failed for sub_task_id={}: {}",
                            entry.sub_task_id,
                            e
                        );
                    }
                }
            }
        }
    });
}
