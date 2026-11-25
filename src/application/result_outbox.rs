use std::sync::Arc;
use std::time::Duration;

use crate::application::state::AppState;
use crate::domain::models::{ComputeTask, TaskResultPayload};
use crate::domain::result::{ResultMessage, MAX_LARGE_BODY};
use crate::infra::storage::{PendingResult, Storage};
use crate::transport::p2p::result_delivery::{build_result_wire_body, send_result_wire};

const DEFAULT_RETRY_INTERVAL_SECS: u64 = 5;

/// The failure to send instead of a body over [`MAX_LARGE_BODY`], which the
/// proxy would drop on every retry.
fn oversize_failure(
    body: &[u8],
    sub_task_id: &str,
    node_id: &str,
) -> anyhow::Result<Option<Vec<u8>>> {
    if body.len() <= MAX_LARGE_BODY {
        return Ok(None);
    }
    let error = format!("result of {} bytes is over the 32 MiB limit", body.len());
    tracing::error!("[Result Outbox] sub_task_id={sub_task_id}: {error}");
    crate::infra::metrics::record_result_oversize();
    ResultMessage::failure_json_bytes(sub_task_id, node_id, &error).map(Some)
}

/// Queues the wire body and records the task's status. Returns the body and
/// whether it reports a success.
fn queue_result(
    storage: &Storage,
    node_id: &str,
    task: &ComputeTask,
    payload: &TaskResultPayload,
) -> anyhow::Result<(Vec<u8>, bool)> {
    let sub_task_id = &task.request.task_id;
    let mut wire_body = build_result_wire_body(task, payload, node_id)?;
    let mut succeeded = payload.status == "success";
    if let Some(failure) = oversize_failure(&wire_body, sub_task_id, node_id)? {
        wire_body = failure;
        succeeded = false;
    }
    storage.upsert_pending_result(&task.orchestrator_pubkey, sub_task_id, &wire_body)?;
    set_status(
        storage,
        &task.orchestrator_pubkey,
        sub_task_id,
        if succeeded { "completed" } else { "failed" },
    );
    Ok((wire_body, succeeded))
}

/// Turns an oversize body that a version without the check queued into a
/// failure, in the outbox and in the task's status.
fn replace_oversize(
    storage: &Storage,
    node_id: &str,
    entry: &mut PendingResult,
) -> anyhow::Result<()> {
    let Some(failure) = oversize_failure(&entry.wire_body, &entry.sub_task_id, node_id)? else {
        return Ok(());
    };
    storage.upsert_pending_result(&entry.orchestrator_pubkey, &entry.sub_task_id, &failure)?;
    set_status(
        storage,
        &entry.orchestrator_pubkey,
        &entry.sub_task_id,
        "failed",
    );
    entry.wire_body = failure;
    Ok(())
}

fn set_status(storage: &Storage, orchestrator_pubkey: &str, sub_task_id: &str, status: &str) {
    if let Err(e) = storage.update_status(orchestrator_pubkey, sub_task_id, status) {
        tracing::error!(
            "Storage status update failed for sub_task_id={sub_task_id} owned by {orchestrator_pubkey}: {e}"
        );
    }
}

/// Persists the P2P wire payload, updates task status, and attempts delivery.
pub async fn deliver_result(
    state: Arc<AppState>,
    task: &ComputeTask,
    payload: &TaskResultPayload,
) -> anyhow::Result<()> {
    let sub_task_id = task.request.task_id.clone();
    let orchestrator_pubkey = task.orchestrator_pubkey.clone();

    let (wire_body, succeeded) =
        queue_result(&state.storage, &state.config.peer_id, task, payload)?;

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
            if let Some(payload) = payload {
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
                if let Err(e) = replace_oversize(&state.storage, &state.config.peer_id, &mut entry)
                {
                    tracing::error!("[Result Outbox] {e}");
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
    use crate::domain::models::{ComplexResult, Proof};

    fn task() -> ComputeTask {
        ComputeTask {
            request: serde_json::from_value(serde_json::json!({
                "task_id": "s", "circuit_id": "c", "qubit_count": 2, "original_qubit_count": 2,
                "slice_id": "0", "slice_assignments": [], "circuit": [],
            }))
            .expect("request"),
            orchestrator_pubkey: "orch".into(),
            budget: None,
            accepted_at_ms: 0,
        }
    }

    fn success(proof_bytes: usize) -> TaskResultPayload {
        TaskResultPayload {
            task_id: "s".into(),
            status: "success".into(),
            result_type: "statevector_scalar".into(),
            complex_result: Some(ComplexResult {
                real: 1.0,
                imag: 0.0,
            }),
            sample_result: None,
            expectation_result: None,
            proof: Some(
                serde_json::from_value::<Proof>(serde_json::json!({
                    "public_inputs": {
                        "circuit_id": "c", "sub_task_id": "s", "node_id": "n",
                        "slice_id": "0", "output_result_hash": "",
                    },
                    "stark_proof_b64": "A".repeat(proof_bytes),
                }))
                .expect("proof"),
            ),
            error: None,
            execution_time_ms: None,
            work_report: None,
        }
    }

    fn storage() -> Storage {
        crate::infra::metrics::init();
        let storage = Storage::new(":memory:").expect("storage");
        storage.save_task(&task()).expect("task");
        storage
    }

    /// (pending, completed, failed) tasks.
    fn counts(storage: &Storage) -> (u64, u64, u64) {
        storage.count_tasks_by_status().expect("counts")
    }

    #[test]
    fn a_body_at_the_limit_passes() {
        crate::infra::metrics::init();
        let body = |n| oversize_failure(&vec![b'x'; n], "s", "n").expect("failure");
        assert!(body(MAX_LARGE_BODY).is_none());
        assert!(body(MAX_LARGE_BODY + 1).is_some());
    }

    #[test]
    fn an_oversize_result_is_queued_as_a_failure() {
        let storage = storage();
        let (body, succeeded) = queue_result(&storage, "n", &task(), &success(16)).expect("ok");
        assert!(succeeded && extract_proof_from_result_wire(&body).unwrap().is_some());
        assert_eq!(counts(&storage), (0, 1, 0));

        let (body, succeeded) =
            queue_result(&storage, "n", &task(), &success(MAX_LARGE_BODY)).expect("ok");
        assert!(!succeeded);
        assert_eq!(counts(&storage), (0, 0, 1));
        let pending = storage.list_pending_results().expect("pending");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].wire_body, body);
        let v: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(
            (&v["sub_task_id"], &v["node_id"]),
            (&"s".into(), &"n".into())
        );
        assert!(v["error"]
            .as_str()
            .unwrap()
            .contains("over the 32 MiB limit"));
        assert!(extract_proof_from_result_wire(&body).unwrap().is_none());
    }

    #[test]
    fn a_queued_oversize_body_is_replaced() {
        let storage = storage();
        storage.update_status("orch", "s", "completed").unwrap();
        storage
            .upsert_pending_result("orch", "s", &vec![b'x'; MAX_LARGE_BODY + 1])
            .unwrap();
        let mut entry = storage.list_pending_results().unwrap().remove(0);
        replace_oversize(&storage, "n", &mut entry).expect("replaced");
        assert!(entry.wire_body.len() < 1024);
        assert_eq!(
            storage.list_pending_results().unwrap()[0].wire_body,
            entry.wire_body
        );
        assert_eq!(counts(&storage), (0, 0, 1));
    }
}
