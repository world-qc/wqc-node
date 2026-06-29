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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicInputs {
    pub circuit_id: String,
    pub sub_task_id: String,
    pub node_id: String,
    pub slice_id: String,
    pub output_result_hash: String,
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
}
