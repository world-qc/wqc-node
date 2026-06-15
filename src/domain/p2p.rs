use serde::Deserialize;

use crate::domain::models::{Gate, SliceAssignment};

pub const ANNOUNCEMENT_TOPIC: &str = "wqc-global-announcements";
pub const PROTOCOL_ANNOUNCE: &str = "/wqc/task-announce/1.0.0";
pub const PROTOCOL_DISPATCH: &str = "/wqc/tensor-dispatch/1.0.0";

/// TaskAnnouncement mirrors the orchestrator gossip payload.
#[derive(Debug, Clone, Deserialize)]
pub struct TaskAnnouncement {
    pub task_id: String,
    pub global_qubit_count: u32,
    pub security_level: String,
    pub required_features: u32,
    pub bid_difficulty: u32,
    pub nonce: u64,
}

/// SubTask mirrors the orchestrator dispatch payload.
#[derive(Debug, Clone, Deserialize)]
pub struct SubTask {
    pub task_id: String,
    pub parent_task_id: String,
    pub circuit_id: String,
    pub qubit_count: u32,
    pub original_qubit_count: u32,
    pub slice_id: String,
    #[serde(default)]
    pub slice_assignments: Vec<SliceAssignment>,
    #[serde(default)]
    pub circuit: Vec<Gate>,
    pub required_votes: u32,
}

#[derive(Debug, Deserialize)]
pub struct TaskDispatchMessage {
    pub sub_task: SubTask,
}

impl SubTask {
    pub fn into_compute_request(self, peer_id: &str) -> crate::domain::models::ComputeRequest {
        crate::domain::models::ComputeRequest {
            task_id: self.task_id,
            parent_task_id: Some(self.parent_task_id),
            circuit_id: self.circuit_id,
            node_id: Some(peer_id.to_string()),
            qubit_count: self.qubit_count as usize,
            original_qubit_count: self.original_qubit_count as usize,
            slice_id: self.slice_id,
            slice_assignments: self.slice_assignments,
            circuit: self.circuit,
            required_votes: Some(self.required_votes),
        }
    }
}
