use crate::domain::models::Proof;
use serde::{Deserialize, Serialize};

pub const PROTOCOL_PCS: &str = "/wqc/tensor-pcs/1.0.0";

/// PCS follow-up message mirrored by the orchestrator P2P handler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PcsMessage {
    pub sub_task_id: String,
    pub node_id: String,
    pub leaf_pcs_b64: String,
}

impl PcsMessage {
    pub fn to_json_bytes(&self) -> anyhow::Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }
}

/// Job persisted until PCS is built and delivered to the orchestrator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingPcsJob {
    pub sub_task_id: String,
    pub proof: Proof,
    /// Cached after a successful `POST /leaf_pcs` so retries only re-send.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leaf_pcs_b64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leaf_pcs_bytes: Option<u64>,
}
