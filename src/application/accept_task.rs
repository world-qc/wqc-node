use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::application::ports::TaskIngress;
use crate::application::state::AppState;
use crate::domain::models::{ComputeRequest, ComputeTask};
use crate::domain::validation::validate_circuit_logic;
use crate::domain::validation::normalize_gate_params;

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

    state.storage.save_task(&task).map_err(|e| {
        AcceptTaskError::Storage(format!(
            "task already exists or storage error: {e}"
        ))
    })?;

    state.pending_tasks.fetch_add(1, Ordering::SeqCst);
    if state.enqueue(task).await.is_err() {
        state.pending_tasks.fetch_sub(1, Ordering::SeqCst);
        return Err(AcceptTaskError::QueueFull);
    }

    Ok(())
}
