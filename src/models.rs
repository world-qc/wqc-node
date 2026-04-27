use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct Gate {
    pub r#type: String, // Use r#type because 'type' is a reserved keyword in Rust
    pub params: serde_json::Value, // Can be i32 or Vec<i32>
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ComputeRequest {
    pub task_id: String,
    pub orchestrator_pubkey: Option<String>,
    pub qubit_count: usize,
    pub circuit: Vec<Gate>,
    pub difficulty: u32,
    pub memory_cost_kb: u32,
    pub webhook_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Proof {
    pub nonce: u64,
    pub proof_hash: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ComputeResponse {
    pub task_id: String,
    pub status: String,
    pub state_vector: Vec<[f64; 2]>,
    pub proof: Proof,
}

#[derive(Debug, Serialize)]
pub struct WebhookPayload {
    pub task_id: String,
    pub status: String,
    pub state_vector: Option<Vec<[f64; 2]>>,
    pub proof: Option<Proof>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct NodeStatus {
    pub node_public_key: String,
    pub pending_tasks: usize,
    pub max_qubits: usize,
    pub max_memory_kb: u32,
    pub system_memory_used_kb: u64,
    pub system_memory_total_kb: u64,
    pub cpu_usage_percent: f32,
    pub supported_gates: Vec<String>,
}
