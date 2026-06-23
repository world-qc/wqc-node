use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::application::ports::TaskIngress;
use crate::application::state::AppState;
use crate::domain::models::{ComputeRequest, ComputeTask};
use crate::domain::validation::validate_circuit_logic;
use crate::domain::validation::normalize_gate_params;

fn dispatch_payload_matches(existing: &ComputeTask, incoming: &ComputeTask) -> bool {
    if existing.orchestrator_pubkey != incoming.orchestrator_pubkey {
        return false;
    }
    let mut existing_req = existing.request.clone();
    let mut incoming_req = incoming.request.clone();
    existing_req.node_id = None;
    incoming_req.node_id = None;
    match (serde_json::to_vec(&existing_req), serde_json::to_vec(&incoming_req)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn is_unique_violation(err: &impl std::fmt::Display) -> bool {
    err.to_string().contains("UNIQUE constraint failed")
}

fn log_duplicate_dispatch(task_id: &str) {
    tracing::debug!(
        sub_task_id = %task_id,
        "Ignoring duplicate SubTask dispatch (already accepted)"
    );
}

#[derive(Debug)]
pub enum AcceptTaskError {
    Validation(String),
    Storage(String),
    QueueFull,
}

impl std::fmt::Display for AcceptTaskError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AcceptTaskError::Validation(msg) => write!(f, "{msg}"),
            AcceptTaskError::Storage(msg) => write!(f, "{msg}"),
            AcceptTaskError::QueueFull => write!(f, "worker queue is full"),
        }
    }
}

impl std::error::Error for AcceptTaskError {}

pub async fn accept_compute_task(
    state: &Arc<AppState>,
    mut request: ComputeRequest,
    orchestrator_pubkey: &str,
) -> Result<(), AcceptTaskError> {
    if request.qubit_count > state.config.max_qubits {
        return Err(AcceptTaskError::Validation(
            "Requested qubit_count exceeds node limit".to_string(),
        ));
    }

    validate_circuit_logic(&request.circuit, request.qubit_count, &state.supported_gates)
        .map_err(AcceptTaskError::Validation)?;

    normalize_gate_params(&mut request.circuit);
    request.node_id = Some(state.config.peer_id.clone());

    let task = ComputeTask {
        request,
        orchestrator_pubkey: orchestrator_pubkey.to_string(),
    };

    if let Some(existing) = state
        .storage
        .load_task(orchestrator_pubkey, &task.request.task_id)
        .map_err(|e| AcceptTaskError::Storage(format!("task lookup failed: {e}")))?
    {
        if dispatch_payload_matches(&existing, &task) {
            log_duplicate_dispatch(&task.request.task_id);
            return Ok(());
        }
        return Err(AcceptTaskError::Storage(format!(
            "task_id {} already exists with a different payload",
            task.request.task_id
        )));
    }

    if let Err(e) = state.storage.save_task(&task) {
        if is_unique_violation(&e) {
            if let Ok(Some(existing)) =
                state.storage.load_task(orchestrator_pubkey, &task.request.task_id)
            {
                if dispatch_payload_matches(&existing, &task) {
                    log_duplicate_dispatch(&task.request.task_id);
                    return Ok(());
                }
            }
        }
        return Err(AcceptTaskError::Storage(format!("storage error: {e}")));
    }

    state.pending_tasks.fetch_add(1, Ordering::SeqCst);
    if state.enqueue(task).await.is_err() {
        state.pending_tasks.fetch_sub(1, Ordering::SeqCst);
        return Err(AcceptTaskError::QueueFull);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::models::Gate;

    fn sample_task(task_id: &str) -> ComputeTask {
        ComputeTask {
            request: ComputeRequest {
                task_id: task_id.to_string(),
                parent_task_id: Some("parent-1".to_string()),
                circuit_id: "circuit-1".to_string(),
                node_id: Some("node-a".to_string()),
                qubit_count: 2,
                original_qubit_count: 3,
                slice_id: "0".to_string(),
                slice_assignments: vec![],
                circuit: vec![Gate {
                    r#type: "H".to_string(),
                    params: serde_json::json!([0]),
                }],
                required_votes: Some(2),
            },
            orchestrator_pubkey: "orch-pubkey".to_string(),
        }
    }

    #[test]
    fn dispatch_payload_matches_ignores_node_id() {
        let mut other = sample_task("sub-1");
        other.request.node_id = Some("node-b".to_string());
        assert!(dispatch_payload_matches(&sample_task("sub-1"), &other));
    }

    #[test]
    fn dispatch_payload_rejects_different_circuit() {
        let mut other = sample_task("sub-1");
        other.request.circuit_id = "other-circuit".to_string();
        assert!(!dispatch_payload_matches(&sample_task("sub-1"), &other));
    }
}
