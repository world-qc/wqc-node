use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::application::ports::TaskIngress;
use crate::application::state::AppState;
use crate::domain::models::{ComputeRequest, ComputeTask};
use crate::domain::validation::validate_circuit_logic;
use crate::domain::work::Budget;

fn dispatch_payload_matches(existing: &ComputeTask, incoming: &ComputeTask) -> bool {
    if existing.orchestrator_pubkey != incoming.orchestrator_pubkey {
        return false;
    }
    let mut existing_req = existing.request.clone();
    let mut incoming_req = incoming.request.clone();
    existing_req.node_id = None;
    incoming_req.node_id = None;
    match (
        serde_json::to_vec(&existing_req),
        serde_json::to_vec(&incoming_req),
    ) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

pub fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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

/// Scalar slices use the χ=128 MPS width. Other modes stay on the dense envelope.
/// A zero MPS width falls back to the dense cap so an old config still accepts work.
pub fn compact_qubit_limit(output_mode: &str, max_qubits: usize, max_mps_qubits: usize) -> usize {
    match output_mode {
        "" | "statevector_scalar" if max_mps_qubits > 0 => max_mps_qubits,
        _ => max_qubits,
    }
}

/// A v2 leaf larger than this node advertised would run out of memory or time, and the node
/// would be slashed for it; refuse it instead (docs `p2p-protocols.md` §5). The node budgets
/// the leaf's effective rows (`wqc_sv_stark::leaf::effective_rows`), which count its basis
/// states above 16 qubits; such a leaf also needs bit 9 (`wide`).
fn check_sv_leaf_rows(
    rows: u64,
    leaf_qubits: usize,
    sv_max_rows: u64,
    wide: bool,
) -> Result<(), AcceptTaskError> {
    if leaf_qubits > 16 && !wide {
        return Err(AcceptTaskError::Validation(format!(
            "v2 leaf on {leaf_qubits} qubits, but this node proves at most 16"
        )));
    }
    let effective = wqc_sv_stark::leaf::effective_rows(leaf_qubits, rows as usize);
    if effective > sv_max_rows {
        return Err(AcceptTaskError::Validation(format!(
            "v2 leaf of {rows} gate rows on {leaf_qubits} qubits ({effective} effective) exceeds this node's sv_max_rows {sv_max_rows}"
        )));
    }
    Ok(())
}

/// The qubits of the leaf the circuit becomes (`wqc_sv_stark::wire::parse_leaf`): the
/// register plus one ancilla per RESET that needs one. A circuit the reader refuses counts as
/// its register; core refuses it anyway.
fn sv_leaf_qubits(circuit: &[crate::domain::models::Gate], qubit_count: usize) -> usize {
    serde_json::to_string(circuit)
        .ok()
        .and_then(|json| wqc_sv_stark::wire::parse_leaf(&json, qubit_count).ok())
        .map_or(qubit_count, |c| c.qubits)
}

/// A tail (`sv_segment`) is only ever a v2 leaf on a node that proves tails: without
/// `sv_leaf_rows` core would run it as v1 over the whole circuit, and a node without
/// `sv_segments` did not bid for it.
fn check_sv_segment(
    has_segment: bool,
    rows: Option<u64>,
    sv_segments: bool,
) -> Result<(), AcceptTaskError> {
    if !has_segment {
        return Ok(());
    }
    if rows.unwrap_or(0) == 0 {
        return Err(AcceptTaskError::Validation(
            "sv_segment without sv_leaf_rows".to_string(),
        ));
    }
    if !sv_segments {
        return Err(AcceptTaskError::Validation(
            "sv_segment, but this node does not prove tails".to_string(),
        ));
    }
    Ok(())
}

pub async fn accept_compute_task(
    state: &Arc<AppState>,
    mut request: ComputeRequest,
    orchestrator_pubkey: &str,
    budget: Option<Budget>,
) -> Result<(), AcceptTaskError> {
    let limit = compact_qubit_limit(
        &request.output_mode,
        state.config.max_qubits,
        state.config.max_mps_qubits,
    );
    if request.qubit_count > limit {
        return Err(AcceptTaskError::Validation(
            "Requested qubit_count exceeds node limit".to_string(),
        ));
    }

    if let Some(rows) = request.sv_leaf_rows {
        let qubits = sv_leaf_qubits(&request.circuit, request.qubit_count);
        check_sv_leaf_rows(rows, qubits, state.config.sv_max_rows, state.config.sv_wide)?;
    }
    check_sv_segment(
        request.sv_segment.is_some(),
        request.sv_leaf_rows,
        state.config.sv_segments,
    )?;

    validate_circuit_logic(
        &request.circuit,
        request.qubit_count,
        &state.supported_gates,
    )
    .map_err(AcceptTaskError::Validation)?;

    request.node_id = Some(state.config.peer_id.clone());

    let task = ComputeTask {
        request,
        orchestrator_pubkey: orchestrator_pubkey.to_string(),
        budget,
        accepted_at_ms: unix_now_ms(),
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
            if let Ok(Some(existing)) = state
                .storage
                .load_task(orchestrator_pubkey, &task.request.task_id)
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
    let sub_task_id = task.request.task_id.clone();
    state.work.lock().expect("work ledger").hold(
        &sub_task_id,
        task.request.parent_task_id.as_deref(),
        task.budget.as_ref(),
    );
    if state.enqueue(task).await.is_err() {
        state.pending_tasks.fetch_sub(1, Ordering::SeqCst);
        state
            .work
            .lock()
            .expect("work ledger")
            .release(&sub_task_id);
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
                mps_max_bond_dim: None,
                mps_site_order: None,
                mps_time_split: None,
                contraction_tree: None,
                output_mode: String::new(),
                classical_bit_count: None,
                shots: None,
                sample_seed: None,
                observables: vec![],
                noise_model: None,
                security_level: String::new(),
                sv_leaf_rows: None,
                sv_segment: None,
            },
            orchestrator_pubkey: "orch-pubkey".to_string(),
            budget: None,
            accepted_at_ms: 0,
        }
    }

    #[test]
    fn scalar_limit_uses_mps_width_and_other_modes_stay_dense() {
        assert_eq!(compact_qubit_limit("statevector_scalar", 26, 2048), 2048);
        assert_eq!(compact_qubit_limit("", 26, 2048), 2048);
        assert_eq!(compact_qubit_limit("expectation", 26, 2048), 26);
        assert_eq!(compact_qubit_limit("sample_counts", 26, 2048), 26);
        assert_eq!(compact_qubit_limit("statevector_scalar", 26, 0), 26);
    }

    #[test]
    fn refuses_v2_leaves_over_sv_max_rows() {
        assert!(check_sv_leaf_rows(1 << 18, 8, 1 << 18, false).is_ok());
        assert!(check_sv_leaf_rows((1 << 18) + 1, 8, 1 << 18, false).is_err());
        // Above 16 qubits: bit 9, and the states count: 2^16 rows on 17 qubits are 2^18.
        assert!(check_sv_leaf_rows(1 << 16, 17, 1 << 18, false).is_err());
        assert!(check_sv_leaf_rows(1 << 16, 17, 1 << 18, true).is_ok());
        assert!(check_sv_leaf_rows(1 << 16, 17, (1 << 18) - 1, true).is_err());
        assert!(
            check_sv_leaf_rows(1, 8, 0, false).is_err(),
            "a node without v2 refuses every leaf"
        );
    }

    #[test]
    fn refuses_a_tail_without_rows_or_tail_support() {
        assert!(check_sv_segment(false, None, false).is_ok());
        assert!(check_sv_segment(true, Some(1 << 10), true).is_ok());
        assert!(check_sv_segment(true, None, true).is_err());
        assert!(check_sv_segment(true, Some(0), true).is_err());
        assert!(check_sv_segment(true, Some(1 << 10), false).is_err());
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
