use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gate {
    pub r#type: String,
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SliceAssignment {
    pub edge_id: String,
    pub value: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoiseModel {
    #[serde(default)]
    pub depolarizing_p: Option<f64>,
    #[serde(default)]
    pub readout_error: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputeRequest {
    pub task_id: String,
    pub parent_task_id: Option<String>,
    pub circuit_id: String,
    pub node_id: Option<String>,
    pub qubit_count: usize,
    pub original_qubit_count: usize,
    pub slice_id: String,
    pub slice_assignments: Vec<SliceAssignment>,
    pub circuit: Vec<Gate>,
    pub required_votes: Option<u32>,
    #[serde(default)]
    pub mps_max_bond_dim: Option<usize>,
    /// Optional MPS path: `site_order[site] = logical` compact qubit (identity if absent).
    #[serde(default)]
    pub mps_site_order: Option<Vec<usize>>,
    /// Gate index of a scalar time split. Absent means one MPS sweep.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mps_time_split: Option<usize>,
    /// Exact contraction tree planned by the orchestrator. Absent keeps the MPS sweep.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contraction_tree: Option<ContractionTreeWire>,
    #[serde(default)]
    pub output_mode: String,
    #[serde(default)]
    pub classical_bit_count: Option<u32>,
    #[serde(default)]
    pub shots: Option<u64>,
    #[serde(default)]
    pub sample_seed: Option<u64>,
    #[serde(default)]
    pub observables: Vec<ObservableSpec>,
    #[serde(default)]
    pub noise_model: Option<NoiseModel>,
    /// Orchestrator security tier (`low|normal|high|ultra`); empty → FRI default.
    #[serde(default)]
    pub security_level: String,
    /// Gate rows of a v2 (sv-stark) leaf; core proves v2 when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sv_leaf_rows: Option<u64>,
}

/// Exact scalar tree. Field order matches the orchestrator's signed JSON.
/// The node forwards it and does not build another plan. Every key the
/// orchestrator can send is here: a key serde dropped would be missing when
/// the node re-encodes the tree, and the signature would fail.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContractionTreeWire {
    /// `network` for a whole-circuit tensor network. Absent is the state tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub nodes: Vec<ContractNodeWire>,
    pub peak_bytes: u64,
}

/// One `contract` node. `op` is `contract`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContractNodeWire {
    pub op: String,
    pub left: TensorRefWire,
    pub right: TensorRefWire,
}

/// One operand. Exactly one of `ket0`, `gate`, `node`, `bra0`, `control`, or
/// `target` is set. `bra0` and the gate halves `control` and `target` appear
/// only in a network tree. The field order is the orchestrator's JSON order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TensorRefWire {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ket0: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bra0: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplexCoeff {
    pub real: f64,
    pub imag: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PauliTerm {
    pub label: String,
    pub coeff: ComplexCoeff,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservableSpec {
    pub id: String,
    pub terms: Vec<PauliTerm>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpectationResult {
    pub values: BTreeMap<String, ComplexResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputeTask {
    pub request: ComputeRequest,
    pub orchestrator_pubkey: String,
    /// The work budget this node bid with, when the announcement carried one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<crate::domain::work::Budget>,
    /// Unix milliseconds when the node accepted the dispatch. The deadline
    /// counts from then, so queueing time comes off it. 0 on a task stored
    /// before this field existed.
    #[serde(default)]
    pub accepted_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicInputs {
    pub circuit_id: String,
    pub sub_task_id: String,
    pub node_id: String,
    pub slice_id: String,
    pub output_result_hash: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub measurement_spec_hash: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub security_level: String,
    /// A v2 leaf's terminal-state digest (4 Goldilocks words). Kept through the node's
    /// re-encoding: the orchestrator verifies the v2 proof against it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sv_digest: Vec<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proof {
    pub public_inputs: PublicInputs,
    pub stark_proof_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkReport {
    pub trace_rows: u64,
    pub gate_count: u32,
    pub compute_wall_ms: u64,
    pub prove_wall_ms: u64,
    pub proof_bytes: u64,
    #[serde(default)]
    pub tn_backend: String,
    #[serde(default)]
    pub vram_peak_bytes: u64,
    /// Time-split scalars mark that the STARK does not bind the overlap.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub proof_scope: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SampleResult {
    pub counts: std::collections::BTreeMap<String, u64>,
    pub shots: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ComputeResponse {
    pub task_id: String,
    pub status: String,
    #[serde(default)]
    pub result_type: String,
    pub complex_result: ComplexResult,
    #[serde(default)]
    pub sample_result: Option<SampleResult>,
    #[serde(default)]
    pub expectation_result: Option<ExpectationResult>,
    pub proof: Proof,
    #[serde(default)]
    pub work_report: Option<WorkReport>,
}

#[derive(Debug, Serialize)]
pub struct TaskResultPayload {
    pub task_id: String,
    pub status: String,
    #[serde(default)]
    pub result_type: String,
    pub complex_result: Option<ComplexResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_result: Option<SampleResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expectation_result: Option<ExpectationResult>,
    pub proof: Option<Proof>,
    pub error: Option<String>,
    pub execution_time_ms: Option<u64>,
    pub work_report: Option<WorkReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplexResult {
    pub real: f64,
    pub imag: f64,
}

#[derive(Debug, Serialize)]
pub struct NodeStatus {
    pub pending_tasks: usize,
    pub outbox_pending: usize,
    pub max_qubits: usize,
    /// χ=128 MPS compact width from the same memory budget as `max_qubits`.
    pub max_mps_qubits: usize,
    pub max_memory_gib: f64,
    /// `ram` or `vram` — which envelope limited the dense bid budget.
    pub memory_cap_source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vram_budget_gib: Option<f64>,
    pub system_memory_used_kb: u64,
    pub system_memory_total_kb: u64,
    pub cpu_usage_percent: f32,
    pub supported_gates: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct CoreSystemInfo {
    pub system_memory_used_kb: u64,
    pub system_memory_total_kb: u64,
    pub cpu_usage_percent: f32,
    #[serde(default)]
    pub tn_backend_requested: String,
    #[serde(default)]
    pub tn_backend_active: String,
    #[serde(default)]
    pub tn_backend_note: Option<String>,
    #[serde(default)]
    pub mps_max_bond_dim: usize,
    /// Prove-time PCS RAM gate on the connected wqc-core (`WQC_PCS_MEMORY_POLICY`).
    #[serde(default = "default_pcs_memory_policy")]
    pub pcs_memory_policy: String,
    /// Adapter `max_buffer_size` when core WebGPU is active.
    #[serde(default)]
    pub vram_budget_bytes: Option<u64>,
    /// Contraction rates measured by core at startup; absent on an older core.
    #[serde(default)]
    pub tn_seconds_per_byte: Option<f64>,
    #[serde(default)]
    pub tn_seconds_per_flop: Option<f64>,
    /// v2 leaf proving rate and memory estimate; absent when core does not prove v2.
    #[serde(default)]
    pub sv_seconds_per_row: Option<f64>,
    #[serde(default)]
    pub sv_bytes_per_row: Option<u64>,
    #[serde(default)]
    pub sv_base_bytes: Option<u64>,
}

fn default_pcs_memory_policy() -> String {
    crate::domain::pcs::PCS_MEMORY_POLICY_REFUSE.to_string()
}

#[cfg(test)]
mod tests {
    use super::WorkReport;

    fn sample_report(proof_scope: &str) -> WorkReport {
        WorkReport {
            trace_rows: 4,
            gate_count: 1,
            compute_wall_ms: 1,
            prove_wall_ms: 1,
            proof_bytes: 1,
            tn_backend: String::new(),
            vram_peak_bytes: 0,
            proof_scope: proof_scope.to_string(),
        }
    }

    #[test]
    fn work_report_omits_empty_proof_scope() {
        let json = serde_json::to_string(&sample_report("")).expect("ser");
        assert!(!json.contains("proof_scope"), "{json}");
    }

    #[test]
    fn work_report_emits_nonempty_proof_scope() {
        let json = serde_json::to_string(&sample_report("mps_time_split")).expect("ser");
        assert!(json.contains(r#""proof_scope":"mps_time_split""#), "{json}");
    }

    #[test]
    fn proof_keeps_the_v2_digest_and_omits_an_empty_one() {
        let core = r#"{"public_inputs":{"circuit_id":"c","sub_task_id":"s","node_id":"n","slice_id":"0","output_result_hash":"h","security_level":"low","sv_digest":[1,2,3,18446744069414584320]},"stark_proof_b64":"AA=="}"#;
        let proof: super::Proof = serde_json::from_str(core).expect("v2 proof");
        assert_eq!(
            proof.public_inputs.sv_digest,
            [1, 2, 3, 18446744069414584320]
        );
        assert_eq!(serde_json::to_string(&proof).expect("ser"), core);

        let v1 = r#"{"public_inputs":{"circuit_id":"c","sub_task_id":"s","node_id":"n","slice_id":"0","output_result_hash":"h"},"stark_proof_b64":"AA=="}"#;
        let proof: super::Proof = serde_json::from_str(v1).expect("v1 proof");
        assert_eq!(serde_json::to_string(&proof).expect("ser"), v1);
    }
}
