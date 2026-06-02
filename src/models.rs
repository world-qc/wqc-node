use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gate {
    pub r#type: String, // Use r#type because 'type' is a reserved keyword in Rust
    pub params: serde_json::Value, // Can be i32 or Vec<i32>
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SliceAssignment {
    pub edge_id: String,
    pub value: u8,
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
    pub webhook_url: Option<String>,
}

/// Internal task representation after validation and difficulty calculation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputeTask {
    pub request: ComputeRequest, // Original request data
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

#[derive(Debug, Serialize, Deserialize)]
pub struct ComputeResponse {
    pub task_id: String,
    pub status: String,
    pub complex_result: ComplexResult,
    pub proof: Proof,
}

#[derive(Debug, Serialize)]
pub struct WebhookPayload {
    pub task_id: String,
    pub status: String,
    pub complex_result: Option<ComplexResult>,
    pub proof: Option<Proof>,
    pub error: Option<String>,
    pub execution_time_ms: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ComplexResult {
    pub real: f64,
    pub imag: f64,
}

#[derive(Debug, Serialize)]
pub struct NodeStatus {
    pub pending_tasks: usize,
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
}
