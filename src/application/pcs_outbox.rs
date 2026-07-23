use std::sync::Arc;
use std::time::Duration;

use crate::application::state::AppState;
use crate::domain::models::{ComputeTask, Proof};
use crate::domain::pcs::PendingPcsJob;
use crate::transport::p2p::pcs_delivery::{build_pcs_wire_body, send_pcs_wire};

const DEFAULT_RETRY_INTERVAL_SECS: u64 = 30;

/// Queues deferred leaf PCS build+delivery after a successful result ACK.
/// Failures never roll back the already-delivered result.
pub fn enqueue_after_result(state: Arc<AppState>, task: &ComputeTask, proof: &Proof) {
    let orchestrator_pubkey = task.orchestrator_pubkey.clone();
    let sub_task_id = task.request.task_id.clone();
    let job = PendingPcsJob {
        sub_task_id: sub_task_id.clone(),
        proof: proof.clone(),
    };

    if let Err(e) = state
        .storage
        .upsert_pending_pcs(&orchestrator_pubkey, &sub_task_id, &job)
    {
        tracing::error!(
            "[PCS Outbox] Failed to enqueue sub_task_id={}: {}",
            sub_task_id,
            e
        );
        return;
    }

    tokio::spawn(async move {
        if let Err(e) = try_build_and_deliver(state, &orchestrator_pubkey, &sub_task_id, &job).await
        {
            tracing::warn!(
                "[PCS Outbox] Initial PCS delivery failed for sub_task_id={} (queued for retry): {}",
                sub_task_id,
                e
            );
        }
    });
}

async fn try_build_and_deliver(
    state: Arc<AppState>,
    orchestrator_pubkey: &str,
    sub_task_id: &str,
    job: &PendingPcsJob,
) -> anyhow::Result<()> {
    let pcs = state.core_client.build_leaf_pcs(job.proof.clone()).await?;
    let wire = build_pcs_wire_body(sub_task_id, &state.config.peer_id, &pcs.leaf_pcs_b64)?;
    send_pcs_wire(state.clone(), &wire).await?;
    state
        .storage
        .delete_pending_pcs(orchestrator_pubkey, sub_task_id)?;
    tracing::info!(
        "[PCS Outbox] Delivered leaf PCS for sub_task_id={} ({} bytes)",
        sub_task_id,
        pcs.bytes
    );
    Ok(())
}

pub fn spawn_retry_loop(state: Arc<AppState>) {
    let interval_secs = std::env::var("WQC_PCS_RETRY_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_RETRY_INTERVAL_SECS);

    tokio::spawn(async move {
        let interval = Duration::from_secs(interval_secs);
        tracing::info!(
            "[PCS Outbox] Retry loop started (interval={}s)",
            interval_secs
        );

        loop {
            tokio::time::sleep(interval).await;

            let pending = match state.storage.list_pending_pcs() {
                Ok(entries) => entries,
                Err(e) => {
                    tracing::error!("[PCS Outbox] Failed to list pending PCS jobs: {}", e);
                    continue;
                }
            };

            if pending.is_empty() {
                continue;
            }

            tracing::debug!("[PCS Outbox] Retrying {} pending PCS job(s)", pending.len());

            for entry in pending {
                let job: PendingPcsJob = match serde_json::from_slice(&entry.job_json) {
                    Ok(j) => j,
                    Err(e) => {
                        tracing::error!(
                            "[PCS Outbox] Corrupt job for sub_task_id={}: {}",
                            entry.sub_task_id,
                            e
                        );
                        continue;
                    }
                };

                match try_build_and_deliver(
                    state.clone(),
                    &entry.orchestrator_pubkey,
                    &entry.sub_task_id,
                    &job,
                )
                .await
                {
                    Ok(()) => {
                        tracing::info!(
                            "[PCS Outbox] Retry delivered sub_task_id={} after {} attempt(s)",
                            entry.sub_task_id,
                            entry.attempts + 1
                        );
                    }
                    Err(e) => {
                        if let Err(inc_err) = state.storage.increment_pending_pcs_attempts(entry.id)
                        {
                            tracing::error!(
                                "[PCS Outbox] Failed to increment attempts for sub_task_id={}: {}",
                                entry.sub_task_id,
                                inc_err
                            );
                        }
                        tracing::debug!(
                            "[PCS Outbox] Retry failed for sub_task_id={}: {}",
                            entry.sub_task_id,
                            e
                        );
                    }
                }
            }
        }
    });
}
