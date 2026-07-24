use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::application::state::AppState;
use crate::domain::models::{ComputeTask, Proof};
use crate::domain::pcs::PendingPcsJob;
use crate::transport::p2p::pcs_delivery::{build_pcs_wire_body, send_pcs_wire};

const DEFAULT_RETRY_INTERVAL_SECS: u64 = 30;

/// Jobs currently running build-or-deliver for a sub_task.
/// Without this, the retry loop starts a second prove while the first is in flight.
static IN_FLIGHT: std::sync::OnceLock<Mutex<HashSet<String>>> = std::sync::OnceLock::new();

fn in_flight() -> &'static Mutex<HashSet<String>> {
    IN_FLIGHT.get_or_init(|| Mutex::new(HashSet::new()))
}

fn claim_inflight(sub_task_id: &str) -> bool {
    let mut set = in_flight().lock().expect("pcs in_flight lock");
    set.insert(sub_task_id.to_string())
}

fn release_inflight(sub_task_id: &str) {
    let mut set = in_flight().lock().expect("pcs in_flight lock");
    set.remove(sub_task_id);
}

/// Queues deferred leaf PCS build+delivery after a successful result ACK.
/// Failures never roll back the already-delivered result.
pub fn enqueue_after_result(state: Arc<AppState>, task: &ComputeTask, proof: &Proof) {
    let orchestrator_pubkey = task.orchestrator_pubkey.clone();
    let sub_task_id = task.request.task_id.clone();
    let mut job = PendingPcsJob {
        sub_task_id: sub_task_id.clone(),
        proof: proof.clone(),
        leaf_pcs_b64: None,
        leaf_pcs_bytes: None,
    };
    // Preserve a prior cached build if result ACK path re-enqueues.
    if let Ok(pending) = state.storage.list_pending_pcs() {
        if let Some(entry) = pending
            .iter()
            .find(|e| e.orchestrator_pubkey == orchestrator_pubkey && e.sub_task_id == sub_task_id)
        {
            if let Ok(existing) = serde_json::from_slice::<PendingPcsJob>(&entry.job_json) {
                if existing.leaf_pcs_b64.is_some() {
                    job.leaf_pcs_b64 = existing.leaf_pcs_b64;
                    job.leaf_pcs_bytes = existing.leaf_pcs_bytes;
                }
            }
        }
    }

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
        match try_build_and_deliver(state, &orchestrator_pubkey, &sub_task_id, job).await {
            Ok(DeliverOutcome::Delivered) => {}
            Ok(DeliverOutcome::SkippedInFlight) => {
                tracing::debug!(
                    "[PCS Outbox] Initial spawn skipped sub_task_id={} (already in flight)",
                    sub_task_id
                );
            }
            Err(e) => {
                tracing::warn!(
                    "[PCS Outbox] Initial PCS delivery failed for sub_task_id={} (queued for retry): {}",
                    sub_task_id,
                    e
                );
            }
        }
    });
}

#[derive(Debug, PartialEq, Eq)]
enum DeliverOutcome {
    Delivered,
    SkippedInFlight,
}

async fn try_build_and_deliver(
    state: Arc<AppState>,
    orchestrator_pubkey: &str,
    sub_task_id: &str,
    mut job: PendingPcsJob,
) -> anyhow::Result<DeliverOutcome> {
    if !claim_inflight(sub_task_id) {
        return Ok(DeliverOutcome::SkippedInFlight);
    }

    let result = async {
        let (leaf_pcs_b64, bytes) = if let Some(cached) = job.leaf_pcs_b64.clone() {
            let bytes = job.leaf_pcs_bytes.unwrap_or(0);
            tracing::info!(
                "[PCS Outbox] Reusing cached leaf PCS for sub_task_id={} ({} bytes); skipping core prove",
                sub_task_id,
                bytes
            );
            (cached, bytes)
        } else {
            let pcs = state.core_client.build_leaf_pcs(job.proof.clone()).await?;
            job.leaf_pcs_b64 = Some(pcs.leaf_pcs_b64.clone());
            job.leaf_pcs_bytes = Some(pcs.bytes);
            // Persist before P2P send so a delivery failure does not force a re-prove.
            state
                .storage
                .upsert_pending_pcs(orchestrator_pubkey, sub_task_id, &job)?;
            tracing::info!(
                "[PCS Outbox] Cached leaf PCS for sub_task_id={} ({} bytes)",
                sub_task_id,
                pcs.bytes
            );
            (pcs.leaf_pcs_b64, pcs.bytes)
        };

        let wire = build_pcs_wire_body(sub_task_id, &state.config.peer_id, &leaf_pcs_b64)?;
        send_pcs_wire(state.clone(), &wire).await?;
        state
            .storage
            .delete_pending_pcs(orchestrator_pubkey, sub_task_id)?;
        tracing::info!(
            "[PCS Outbox] Delivered leaf PCS for sub_task_id={} ({} bytes)",
            sub_task_id,
            bytes
        );
        Ok(DeliverOutcome::Delivered)
    }
    .await;

    release_inflight(sub_task_id);
    result
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
                    job,
                )
                .await
                {
                    Ok(DeliverOutcome::Delivered) => {
                        tracing::info!(
                            "[PCS Outbox] Retry delivered sub_task_id={} after {} attempt(s)",
                            entry.sub_task_id,
                            entry.attempts + 1
                        );
                    }
                    Ok(DeliverOutcome::SkippedInFlight) => {
                        tracing::debug!(
                            "[PCS Outbox] Retry skipped sub_task_id={} (build/deliver already in flight)",
                            entry.sub_task_id
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
