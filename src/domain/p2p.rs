use base64::{engine::general_purpose::STANDARD, Engine as _};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Deserialize;
use serde_json;

use crate::domain::models::{Gate, SliceAssignment};

pub const ANNOUNCEMENT_TOPIC: &str = "wqc-global-announcements";
pub const PROTOCOL_ANNOUNCE: &str = "/wqc/task-announce/1.0.0";
pub const PROTOCOL_DISPATCH: &str = "/wqc/tensor-dispatch/1.0.0";

/// TaskAnnouncement mirrors the orchestrator signed gossip payload.
#[derive(Debug, Clone, Deserialize)]
pub struct TaskAnnouncement {
    pub task_id: String,
    pub global_qubit_count: u32,
    pub required_features: u32,
    pub bid_difficulty: u32,
    pub required_votes: u32,
    pub nonce: u64,
}

#[derive(Debug, Deserialize)]
pub struct TaskAnnouncementMessage {
    pub announcement: TaskAnnouncement,
    pub signature: String,
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

/// Mirrors orchestrator `bid.SerializeAnnouncementPayload` byte layout exactly.
pub fn serialize_announcement_payload(announcement: &TaskAnnouncement) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(announcement.task_id.as_bytes());
    payload.extend_from_slice(&announcement.global_qubit_count.to_be_bytes());
    payload.extend_from_slice(&announcement.required_features.to_be_bytes());
    payload.extend_from_slice(&announcement.bid_difficulty.to_be_bytes());
    payload.extend_from_slice(&announcement.required_votes.to_be_bytes());
    payload.extend_from_slice(&announcement.nonce.to_be_bytes());
    payload
}

pub fn parse_signed_announcement(
    payload: &[u8],
    orchestrator_public_key_b64: &str,
) -> Result<TaskAnnouncement, String> {
    let message: TaskAnnouncementMessage = serde_json::from_slice(payload)
        .map_err(|e| format!("invalid announcement JSON: {e}"))?;
    verify_announcement_message(&message, orchestrator_public_key_b64)?;
    Ok(message.announcement)
}

pub fn verify_announcement_message(
    message: &TaskAnnouncementMessage,
    orchestrator_public_key_b64: &str,
) -> Result<(), String> {
    let signed_payload = serialize_announcement_payload(&message.announcement);
    verify_orchestrator_signature(
        &signed_payload,
        &message.signature,
        orchestrator_public_key_b64,
        "announcement",
    )
}

#[derive(Debug, Deserialize)]
pub struct TaskDispatchMessage {
    pub sub_task: SubTask,
    pub signature: String,
}

/// Mirrors orchestrator `task.SerializeDispatchPayload` byte layout exactly.
pub fn serialize_dispatch_payload(sub_task: &SubTask) -> Result<Vec<u8>, String> {
    let mut payload = Vec::new();
    payload.extend_from_slice(sub_task.task_id.as_bytes());
    payload.extend_from_slice(sub_task.parent_task_id.as_bytes());
    payload.extend_from_slice(sub_task.circuit_id.as_bytes());
    payload.extend_from_slice(sub_task.slice_id.as_bytes());
    payload.extend_from_slice(&sub_task.qubit_count.to_be_bytes());
    payload.extend_from_slice(&sub_task.original_qubit_count.to_be_bytes());
    payload.extend_from_slice(&sub_task.required_votes.to_be_bytes());

    let assignments = &sub_task.slice_assignments;
    payload.extend_from_slice(&(assignments.len() as u32).to_be_bytes());
    for assignment in assignments {
        payload.extend_from_slice(assignment.edge_id.as_bytes());
        payload.push(assignment.value);
    }

    let circuit_json = serde_json::to_vec(&sub_task.circuit)
        .map_err(|e| format!("marshal circuit for dispatch signing: {e}"))?;
    payload.extend_from_slice(&(circuit_json.len() as u32).to_be_bytes());
    payload.extend_from_slice(&circuit_json);

    Ok(payload)
}

pub fn verify_dispatch_signature(
    sub_task: &SubTask,
    signature_b64: &str,
    orchestrator_public_key_b64: &str,
) -> Result<(), String> {
    let payload = serialize_dispatch_payload(sub_task)?;
    verify_orchestrator_signature(
        &payload,
        signature_b64,
        orchestrator_public_key_b64,
        "dispatch",
    )
}

fn verify_orchestrator_signature(
    payload: &[u8],
    signature_b64: &str,
    orchestrator_public_key_b64: &str,
    label: &str,
) -> Result<(), String> {
    let signature_bytes = STANDARD
        .decode(signature_b64.trim())
        .map_err(|e| format!("invalid {label} signature base64: {e}"))?;
    let signature = Signature::from_slice(&signature_bytes)
        .map_err(|e| format!("invalid {label} signature bytes: {e}"))?;

    let public_key_bytes = STANDARD
        .decode(orchestrator_public_key_b64.trim())
        .map_err(|e| format!("invalid orchestrator public key base64: {e}"))?;
    let verifying_key = VerifyingKey::from_bytes(
        public_key_bytes
            .as_slice()
            .try_into()
            .map_err(|_| "orchestrator public key must be 32 bytes".to_string())?,
    )
    .map_err(|e| format!("invalid orchestrator public key: {e}"))?;

    verifying_key
        .verify(payload, &signature)
        .map_err(|_| format!("{label} signature verification failed"))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::models::Gate;

    #[test]
    fn serialize_announcement_payload_layout_matches_orchestrator() {
        let announcement = TaskAnnouncement {
            task_id: "task-parent-1".to_string(),
            global_qubit_count: 30,
            required_features: 3,
            bid_difficulty: 2,
            required_votes: 2,
            nonce: 42,
        };

        let payload = serialize_announcement_payload(&announcement);
        let mut expected = Vec::new();
        expected.extend_from_slice(b"task-parent-1");
        expected.extend_from_slice(&30u32.to_be_bytes());
        expected.extend_from_slice(&3u32.to_be_bytes());
        expected.extend_from_slice(&2u32.to_be_bytes());
        expected.extend_from_slice(&2u32.to_be_bytes());
        expected.extend_from_slice(&42u64.to_be_bytes());
        assert_eq!(payload, expected);
    }

    #[test]
    fn serialize_dispatch_payload_layout_matches_orchestrator() {
        let sub_task = SubTask {
            task_id: "sub-task-1".to_string(),
            parent_task_id: "parent-uuid".to_string(),
            circuit_id: "abc123".to_string(),
            qubit_count: 2,
            original_qubit_count: 3,
            slice_id: "01".to_string(),
            slice_assignments: vec![SliceAssignment {
                edge_id: "e_0".to_string(),
                value: 1,
            }],
            circuit: vec![Gate {
                r#type: "H".to_string(),
                params: serde_json::json!([0]),
            }],
            required_votes: 2,
        };

        let payload = serialize_dispatch_payload(&sub_task).expect("serialize dispatch payload");
        let mut expected = Vec::new();
        expected.extend_from_slice(b"sub-task-1");
        expected.extend_from_slice(b"parent-uuid");
        expected.extend_from_slice(b"abc123");
        expected.extend_from_slice(b"01");
        expected.extend_from_slice(&2u32.to_be_bytes());
        expected.extend_from_slice(&3u32.to_be_bytes());
        expected.extend_from_slice(&2u32.to_be_bytes());
        expected.extend_from_slice(&1u32.to_be_bytes());
        expected.extend_from_slice(b"e_0");
        expected.push(1);
        let circuit_json = br#"[{"type":"H","params":[0]}]"#;
        expected.extend_from_slice(&(circuit_json.len() as u32).to_be_bytes());
        expected.extend_from_slice(circuit_json);
        assert_eq!(payload, expected);
    }
}
