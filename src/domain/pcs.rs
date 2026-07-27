use crate::domain::models::Proof;
use serde::{Deserialize, Serialize};

pub const PROTOCOL_PCS: &str = "/wqc/tensor-pcs/1.0.0";
pub const PROTOCOL_PCS_REQUEST: &str = "/wqc/tensor-pcs-req/1.0.0";

/// PCS follow-up message mirrored by the orchestrator P2P handler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PcsMessage {
    pub sub_task_id: String,
    pub node_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub leaf_pcs_b64: String,
    /// Winner permanently declines leaf PCS (e.g. memory gate refuse); orch compose fallback.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub refused: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refuse_reason: Option<String>,
}

impl PcsMessage {
    pub fn to_json_bytes(&self) -> anyhow::Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }
}

/// Orchestrator → slice proof winner: build the deferred leaf PCS for one sub-task.
#[derive(Debug, Clone, Deserialize)]
pub struct PcsRequest {
    pub sub_task_id: String,
    #[serde(default)]
    pub parent_task_id: String,
    #[serde(default)]
    pub slice_id: String,
    #[serde(default)]
    pub node_id: String,
    #[serde(default)]
    pub issued_at_unix: i64,
}

#[derive(Debug, Deserialize)]
pub struct PcsRequestMessage {
    pub request: PcsRequest,
    pub signature: String,
}

/// Mirrors orchestrator `task.SerializePcsRequestPayload` byte layout exactly.
pub fn serialize_pcs_request_payload(request: &PcsRequest) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(request.sub_task_id.as_bytes());
    payload.extend_from_slice(request.parent_task_id.as_bytes());
    payload.extend_from_slice(request.slice_id.as_bytes());
    payload.extend_from_slice(request.node_id.as_bytes());
    payload.extend_from_slice(&request.issued_at_unix.to_be_bytes());
    payload
}

pub fn verify_pcs_request_signature(
    request: &PcsRequest,
    signature_b64: &str,
    orchestrator_public_key_b64: &str,
) -> Result<(), String> {
    let payload = serialize_pcs_request_payload(request);
    crate::domain::p2p::verify_orchestrator_signature(
        &payload,
        signature_b64,
        orchestrator_public_key_b64,
        "pcs request",
    )
}

/// Job persisted after a result ACK, holding the proof the leaf PCS binds to.
/// Proving only starts once the orchestrator names this node the slice proof
/// winner, so a losing node never pays the memory cost of a bundle nobody uses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingPcsJob {
    pub sub_task_id: String,
    pub proof: Proof,
    /// Set once the orchestrator has asked this node for the bundle.
    #[serde(default)]
    pub requested: bool,
    /// Cached after a successful `POST /leaf_pcs` so retries only re-send.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leaf_pcs_b64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leaf_pcs_bytes: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_pcs_request_payload_layout_matches_orchestrator() {
        let request = PcsRequest {
            sub_task_id: "parent_01-sub".to_string(),
            parent_task_id: "parent".to_string(),
            slice_id: "01".to_string(),
            node_id: "12D3KooWnode".to_string(),
            issued_at_unix: 1_700_000_000,
        };

        let payload = serialize_pcs_request_payload(&request);
        let mut expected = Vec::new();
        expected.extend_from_slice(b"parent_01-sub");
        expected.extend_from_slice(b"parent");
        expected.extend_from_slice(b"01");
        expected.extend_from_slice(b"12D3KooWnode");
        expected.extend_from_slice(&1_700_000_000i64.to_be_bytes());
        assert_eq!(payload, expected);
    }

    #[test]
    fn pending_pcs_job_defaults_to_unrequested() {
        let json = r#"{"sub_task_id":"s","proof":{"stark_proof_b64":"","public_inputs":{
            "circuit_id":"c","sub_task_id":"s","node_id":"n","slice_id":"0","output_result_hash":"h"
        }}}"#;
        let job: PendingPcsJob = serde_json::from_str(json).expect("decode legacy job");
        assert!(!job.requested);
    }

    #[test]
    fn pcs_refusal_message_serializes_refused_flag() {
        let msg = PcsMessage {
            sub_task_id: "t_090100".to_string(),
            node_id: "12D3KooWnode".to_string(),
            leaf_pcs_b64: String::new(),
            refused: true,
            refuse_reason: Some(
                "PCS memory: estimate=3.57 GiB exceeds budget=2.00 GiB (policy=refuse)".into(),
            ),
        };
        let json = serde_json::to_string(&msg).expect("serialize");
        assert!(json.contains("\"refused\":true"));
        assert!(json.contains("policy=refuse"));
        let back: PcsMessage = serde_json::from_str(&json).expect("deserialize");
        assert!(back.refused);
        assert!(back.leaf_pcs_b64.is_empty());
    }
}
