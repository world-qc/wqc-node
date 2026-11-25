use std::sync::Arc;
use std::time::Duration;

use crate::application::state::AppState;
use crate::domain::models::{ComputeTask, TaskResultPayload};
use crate::domain::result::ResultMessage;
use crate::transport::p2p::result_delivery::{build_result_wire_body, send_result_wire};

const DEFAULT_RETRY_INTERVAL_SECS: u64 = 5;

/// The largest result body the orchestrator's proxy reads (wqc-docs
/// spec/p2p-protocols.md §2.2).
const MAX_RESULT_BODY: usize = 32 << 20;

/// The failure to send instead of a body over [`MAX_RESULT_BODY`], which the
/// proxy would drop on every retry.
fn oversize_failure(
    body: &[u8],
    sub_task_id: &str,
    node_id: &str,
) -> anyhow::Result<Option<Vec<u8>>> {
    if body.len() <= MAX_RESULT_BODY {
        return Ok(None);
    }
    let error = format!(
        "result of {} bytes is over the {MAX_RESULT_BODY} byte limit",
        body.len()
    );
    tracing::error!("[Result Outbox] sub_task_id={sub_task_id}: {error}");
    ResultMessage::failure_json_bytes(sub_task_id, node_id, &error).map(Some)
}

/// Persists the P2P wire payload, updates task status, and attempts delivery.
pub async fn deliver_result(
    state: Arc<AppState>,
    task: &ComputeTask,
    payload: &TaskResultPayload,
) -> anyhow::Result<()> {
    let sub_task_id = task.request.task_id.clone();
    let orchestrator_pubkey = task.orchestrator_pubkey.clone();

    let mut wire_body = build_result_wire_body(task, payload, &state.config.peer_id)?;
    let mut succeeded = payload.status != "error";
    if let Some(failure) = oversize_failure(&wire_body, &sub_task_id, &state.config.peer_id)? {
        wire_body = failure;
        succeeded = false;
    }

    state
        .storage
        .upsert_pending_result(&orchestrator_pubkey, &sub_task_id, &wire_body)?;

    let status = if succeeded { "completed" } else { "failed" };
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

    try_deliver_pending(
        state,
        &orchestrator_pubkey,
        &sub_task_id,
        &wire_body,
        succeeded.then_some(payload),
        task,
    )
    .await
}

async fn try_deliver_pending(
    state: Arc<AppState>,
    orchestrator_pubkey: &str,
    sub_task_id: &str,
    wire_body: &[u8],
    payload: Option<&TaskResultPayload>,
    task: &ComputeTask,
) -> anyhow::Result<()> {
    match send_result_wire(state.clone(), wire_body).await {
        Ok(()) => {
            state
                .storage
                .delete_pending_result(orchestrator_pubkey, sub_task_id)?;
            crate::application::challenger::watch_after_delivery(
                &state,
                orchestrator_pubkey,
                sub_task_id,
                wire_body,
            );
            crate::infra::metrics::record_result_delivery("initial", "ok");
            tracing::info!(
                "[Result Outbox] Delivered sub_task_id={} to orchestrator",
                sub_task_id
            );
            if let Some(payload) = payload.filter(|p| p.status == "success") {
                if let Some(proof) = payload.proof.as_ref() {
                    crate::application::pcs_outbox::retain_after_result(state.clone(), task, proof);
                }
            }
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

            for mut entry in pending {
                // Queued by a version without the check.
                match oversize_failure(&entry.wire_body, &entry.sub_task_id, &state.config.peer_id)
                {
                    Ok(Some(failure)) => entry.wire_body = failure,
                    Ok(None) => {}
                    Err(e) => tracing::error!("[Result Outbox] {e}"),
                }
                match send_result_wire(state.clone(), &entry.wire_body).await {
                    Ok(()) => {
                        crate::infra::metrics::record_result_delivery("retry", "ok");
                        crate::application::challenger::watch_after_delivery(
                            &state,
                            &entry.orchestrator_pubkey,
                            &entry.sub_task_id,
                            &entry.wire_body,
                        );
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
                        // Retain the proof on the retry path too: decoding it from the
                        // wire is cheap next to re-running compute if we are later
                        // named slice proof winner.
                        if let Ok(Some(task)) = state
                            .storage
                            .load_task(&entry.orchestrator_pubkey, &entry.sub_task_id)
                        {
                            if let Ok(Some(proof)) =
                                extract_proof_from_result_wire(&entry.wire_body)
                            {
                                crate::application::pcs_outbox::retain_after_result(
                                    state.clone(),
                                    &task,
                                    &proof,
                                );
                            }
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

fn extract_proof_from_result_wire(
    wire_body: &[u8],
) -> anyhow::Result<Option<crate::domain::models::Proof>> {
    let value: serde_json::Value = serde_json::from_slice(wire_body)?;
    if value.get("error").and_then(|e| e.as_str()).is_some() {
        return Ok(None);
    }
    let proof = value.get("proof").cloned();
    match proof {
        Some(p) if !p.is_null() => Ok(Some(serde_json::from_value(p)?)),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_oversize_body_becomes_a_failure() {
        assert!(oversize_failure(&vec![b'x'; MAX_RESULT_BODY], "s", "n")
            .unwrap()
            .is_none());
        let body = oversize_failure(&vec![b'x'; MAX_RESULT_BODY + 1], "s", "n")
            .unwrap()
            .expect("over the limit");
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["sub_task_id"], "s");
        assert_eq!(v["node_id"], "n");
        assert!(v["error"].as_str().unwrap().contains("over the"));
        assert!(extract_proof_from_result_wire(&body).unwrap().is_none());
    }
}
