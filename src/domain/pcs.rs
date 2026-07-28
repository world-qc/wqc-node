use crate::domain::models::Proof;
use serde::{Deserialize, Serialize};

pub const PROTOCOL_PCS: &str = "/wqc/tensor-pcs/1.0.0";
pub const PROTOCOL_PCS_REQUEST: &str = "/wqc/tensor-pcs-req/1.0.0";
pub const PROTOCOL_PCS_OPEN: &str = "/wqc/tensor-pcs-open/1.0.0";
pub const PROTOCOL_PCS_BID: &str = "/wqc/tensor-pcs-bid/1.0.0";

/// Empty / omitted `request_kind` means nominated majority PCS request (v1-compatible).
pub const PCS_REQUEST_KIND_OPEN_CALL: &str = "open_call";
/// Only spill-policy nodes may bid on PCS open calls.
pub const PCS_MEMORY_POLICY_SPILL: &str = "spill";
pub const PCS_MEMORY_POLICY_REFUSE: &str = "refuse";

/// Node-local PCS memory gate policy (`WQC_PCS_MEMORY_POLICY`).
/// Spill nodes may bid on CAS open calls; refuse nodes must not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PcsMemoryPolicy {
    Refuse,
    Spill,
}

impl PcsMemoryPolicy {
    pub fn from_env_str(raw: Option<&str>) -> Self {
        match raw.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
            Some(PCS_MEMORY_POLICY_SPILL) => Self::Spill,
            _ => Self::Refuse,
        }
    }

    pub fn from_env() -> Self {
        Self::from_env_str(std::env::var("WQC_PCS_MEMORY_POLICY").ok().as_deref())
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Refuse => PCS_MEMORY_POLICY_REFUSE,
            Self::Spill => PCS_MEMORY_POLICY_SPILL,
        }
    }

    pub fn is_spill(self) -> bool {
        matches!(self, Self::Spill)
    }
}

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

/// Orchestrator → nominated node or open-call builder: build deferred leaf PCS.
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
    /// Empty = nominated/majority (v1-compatible). `"open_call"` = CAS builder.
    #[serde(default)]
    pub request_kind: String,
    #[serde(default)]
    pub leaf_proof_hash: String,
}

impl PcsRequest {
    pub fn is_open_call(&self) -> bool {
        self.request_kind == PCS_REQUEST_KIND_OPEN_CALL
    }
}

#[derive(Debug, Deserialize)]
pub struct PcsRequestMessage {
    pub request: PcsRequest,
    pub signature: String,
}

/// Orchestrator open-call announcement (CAS-backed PCS builder market).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PcsOpenCall {
    pub parent_task_id: String,
    pub sub_task_id: String,
    pub slice_id: String,
    pub leaf_proof_hash: String,
    pub leaf_proof_bytes: u64,
    pub cas_presigned_url: String,
    pub r_pcs_planck: String,
    pub deadline_unix: i64,
    pub issued_at_unix: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refused_builders: Vec<String>,
}

impl PcsOpenCall {
    pub fn is_expired_at(&self, now_unix: i64) -> bool {
        self.deadline_unix > 0 && now_unix >= self.deadline_unix
    }

    pub fn is_expired(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.is_expired_at(now)
    }

    pub fn refuses_builder(&self, node_id: &str) -> bool {
        self.refused_builders.iter().any(|id| id == node_id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PcsOpenCallMessage {
    pub open_call: PcsOpenCall,
    pub signature: String,
}

/// Node → orchestrator offer to build PCS from a CAS leaf proof.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PcsBid {
    pub sub_task_id: String,
    pub node_id: String,
    pub leaf_proof_hash: String,
    pub pcs_memory_policy: String,
    pub issued_at_unix: i64,
}

impl PcsBid {
    pub fn is_spill_policy(&self) -> bool {
        self.pcs_memory_policy == PCS_MEMORY_POLICY_SPILL
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PcsBidMessage {
    pub bid: PcsBid,
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
    payload.extend_from_slice(request.request_kind.as_bytes());
    payload.extend_from_slice(request.leaf_proof_hash.as_bytes());
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

/// Mirrors orchestrator `task.SerializePcsOpenCallPayload` byte layout exactly.
pub fn serialize_pcs_open_call_payload(call: &PcsOpenCall) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(call.parent_task_id.as_bytes());
    payload.extend_from_slice(call.sub_task_id.as_bytes());
    payload.extend_from_slice(call.slice_id.as_bytes());
    payload.extend_from_slice(call.leaf_proof_hash.as_bytes());
    payload.extend_from_slice(&call.leaf_proof_bytes.to_be_bytes());
    payload.extend_from_slice(call.cas_presigned_url.as_bytes());
    payload.extend_from_slice(call.r_pcs_planck.as_bytes());
    payload.extend_from_slice(&call.deadline_unix.to_be_bytes());
    payload.extend_from_slice(&call.issued_at_unix.to_be_bytes());

    let mut refused = call.refused_builders.clone();
    refused.sort();
    payload.extend_from_slice(&(refused.len() as u32).to_be_bytes());
    for id in refused {
        let id_bytes = id.as_bytes();
        payload.extend_from_slice(&(id_bytes.len() as u32).to_be_bytes());
        payload.extend_from_slice(id_bytes);
    }
    payload
}

pub fn verify_pcs_open_call_signature(
    call: &PcsOpenCall,
    signature_b64: &str,
    orchestrator_public_key_b64: &str,
) -> Result<(), String> {
    let payload = serialize_pcs_open_call_payload(call);
    crate::domain::p2p::verify_orchestrator_signature(
        &payload,
        signature_b64,
        orchestrator_public_key_b64,
        "pcs open call",
    )
}

/// Mirrors orchestrator `task.SerializePcsBidPayload` byte layout exactly.
pub fn serialize_pcs_bid_payload(bid: &PcsBid) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(bid.sub_task_id.as_bytes());
    payload.extend_from_slice(bid.node_id.as_bytes());
    payload.extend_from_slice(bid.leaf_proof_hash.as_bytes());
    payload.extend_from_slice(bid.pcs_memory_policy.as_bytes());
    payload.extend_from_slice(&bid.issued_at_unix.to_be_bytes());
    payload
}

/// Builds a signed spill-policy open-call bid for `/wqc/tensor-pcs-bid/1.0.0`.
pub fn build_signed_pcs_bid(
    open: &PcsOpenCall,
    node_id: &str,
    signing_key: &ed25519_dalek::SigningKey,
) -> PcsBidMessage {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use ed25519_dalek::Signer;

    let issued_at_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let bid = PcsBid {
        sub_task_id: open.sub_task_id.clone(),
        node_id: node_id.to_string(),
        leaf_proof_hash: open.leaf_proof_hash.clone(),
        pcs_memory_policy: PCS_MEMORY_POLICY_SPILL.to_string(),
        issued_at_unix,
    };
    let signature = STANDARD.encode(
        signing_key
            .sign(&serialize_pcs_bid_payload(&bid))
            .to_bytes(),
    );
    PcsBidMessage { bid, signature }
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
            request_kind: String::new(),
            leaf_proof_hash: String::new(),
        };

        let payload = serialize_pcs_request_payload(&request);
        let mut expected = Vec::new();
        expected.extend_from_slice(b"parent_01-sub");
        expected.extend_from_slice(b"parent");
        expected.extend_from_slice(b"01");
        expected.extend_from_slice(b"12D3KooWnode");
        expected.extend_from_slice(&1_700_000_000i64.to_be_bytes());
        assert_eq!(payload, expected);
        assert!(!request.is_open_call());
    }

    #[test]
    fn serialize_pcs_request_payload_open_call_appends_kind_and_hash() {
        let hash = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let request = PcsRequest {
            sub_task_id: "parent_01-sub".to_string(),
            parent_task_id: "parent".to_string(),
            slice_id: "01".to_string(),
            node_id: "12D3KooWnode".to_string(),
            issued_at_unix: 1_700_000_000,
            request_kind: PCS_REQUEST_KIND_OPEN_CALL.to_string(),
            leaf_proof_hash: hash.to_string(),
        };

        let payload = serialize_pcs_request_payload(&request);
        let mut expected = Vec::new();
        expected.extend_from_slice(b"parent_01-sub");
        expected.extend_from_slice(b"parent");
        expected.extend_from_slice(b"01");
        expected.extend_from_slice(b"12D3KooWnode");
        expected.extend_from_slice(&1_700_000_000i64.to_be_bytes());
        expected.extend_from_slice(PCS_REQUEST_KIND_OPEN_CALL.as_bytes());
        expected.extend_from_slice(hash.as_bytes());
        assert_eq!(payload, expected);
        assert!(request.is_open_call());
    }

    #[test]
    fn serialize_pcs_open_call_payload_matches_orchestrator() {
        let call = PcsOpenCall {
            parent_task_id: "parent".to_string(),
            sub_task_id: "parent_01-sub".to_string(),
            slice_id: "01".to_string(),
            leaf_proof_hash: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .to_string(),
            leaf_proof_bytes: 42,
            cas_presigned_url: "https://s3.example/blob".to_string(),
            r_pcs_planck: "400000000000".to_string(),
            deadline_unix: 1_700_001_800,
            issued_at_unix: 1_700_000_000,
            refused_builders: vec!["node-b".to_string(), "node-a".to_string()],
        };

        let payload = serialize_pcs_open_call_payload(&call);
        let mut expected = Vec::new();
        expected.extend_from_slice(b"parent");
        expected.extend_from_slice(b"parent_01-sub");
        expected.extend_from_slice(b"01");
        expected.extend_from_slice(call.leaf_proof_hash.as_bytes());
        expected.extend_from_slice(&42u64.to_be_bytes());
        expected.extend_from_slice(b"https://s3.example/blob");
        expected.extend_from_slice(b"400000000000");
        expected.extend_from_slice(&1_700_001_800i64.to_be_bytes());
        expected.extend_from_slice(&1_700_000_000i64.to_be_bytes());
        expected.extend_from_slice(&2u32.to_be_bytes());
        for id in ["node-a", "node-b"] {
            let id_bytes = id.as_bytes();
            expected.extend_from_slice(&(id_bytes.len() as u32).to_be_bytes());
            expected.extend_from_slice(id_bytes);
        }
        assert_eq!(payload, expected);
    }

    #[test]
    fn serialize_pcs_bid_payload_matches_orchestrator() {
        let bid = PcsBid {
            sub_task_id: "parent_01-sub".to_string(),
            node_id: "12D3KooWbuilder".to_string(),
            leaf_proof_hash: "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
                .to_string(),
            pcs_memory_policy: PCS_MEMORY_POLICY_SPILL.to_string(),
            issued_at_unix: 1_700_000_100,
        };

        let payload = serialize_pcs_bid_payload(&bid);
        let mut expected = Vec::new();
        expected.extend_from_slice(bid.sub_task_id.as_bytes());
        expected.extend_from_slice(bid.node_id.as_bytes());
        expected.extend_from_slice(bid.leaf_proof_hash.as_bytes());
        expected.extend_from_slice(PCS_MEMORY_POLICY_SPILL.as_bytes());
        expected.extend_from_slice(&1_700_000_100i64.to_be_bytes());
        assert_eq!(payload, expected);
        assert!(bid.is_spill_policy());
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

    #[test]
    fn pcs_request_legacy_json_defaults_open_call_fields() {
        let json = r#"{
            "sub_task_id":"s",
            "parent_task_id":"p",
            "slice_id":"0",
            "node_id":"n",
            "issued_at_unix":1
        }"#;
        let req: PcsRequest = serde_json::from_str(json).expect("decode");
        assert!(req.request_kind.is_empty());
        assert!(req.leaf_proof_hash.is_empty());
        assert!(!req.is_open_call());
    }

    #[test]
    fn pcs_memory_policy_from_env_str() {
        assert_eq!(
            PcsMemoryPolicy::from_env_str(Some("spill")),
            PcsMemoryPolicy::Spill
        );
        assert_eq!(
            PcsMemoryPolicy::from_env_str(Some("SPILL")),
            PcsMemoryPolicy::Spill
        );
        assert_eq!(
            PcsMemoryPolicy::from_env_str(Some("refuse")),
            PcsMemoryPolicy::Refuse
        );
        assert_eq!(PcsMemoryPolicy::from_env_str(None), PcsMemoryPolicy::Refuse);
    }

    #[test]
    fn open_call_expiry_and_refused_builder() {
        let call = PcsOpenCall {
            parent_task_id: "parent".to_string(),
            sub_task_id: "parent_01-sub".to_string(),
            slice_id: "01".to_string(),
            leaf_proof_hash: "aa".to_string(),
            leaf_proof_bytes: 1,
            cas_presigned_url: "https://x".to_string(),
            r_pcs_planck: "1".to_string(),
            deadline_unix: 100,
            issued_at_unix: 0,
            refused_builders: vec!["node-a".to_string()],
        };
        assert!(!call.is_expired_at(99));
        assert!(call.is_expired_at(100));
        assert!(call.refuses_builder("node-a"));
        assert!(!call.refuses_builder("node-b"));
    }

    #[test]
    fn build_signed_pcs_bid_is_spill_and_verifiable() {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        use ed25519_dalek::{Verifier, VerifyingKey};

        let key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let open = PcsOpenCall {
            parent_task_id: "parent".to_string(),
            sub_task_id: "parent_01-sub".to_string(),
            slice_id: "01".to_string(),
            leaf_proof_hash: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                .to_string(),
            leaf_proof_bytes: 10,
            cas_presigned_url: "https://s3.example/blob".to_string(),
            r_pcs_planck: "1".to_string(),
            deadline_unix: 2_000_000_000,
            issued_at_unix: 1_900_000_000,
            refused_builders: vec![],
        };
        let msg = build_signed_pcs_bid(&open, "12D3KooWbuilder", &key);
        assert!(msg.bid.is_spill_policy());
        assert_eq!(msg.bid.node_id, "12D3KooWbuilder");
        assert_eq!(msg.bid.leaf_proof_hash, open.leaf_proof_hash);

        let sig = STANDARD.decode(&msg.signature).expect("sig b64");
        let verifying = VerifyingKey::from(&key);
        verifying
            .verify(
                &serialize_pcs_bid_payload(&msg.bid),
                &ed25519_dalek::Signature::from_slice(&sig).expect("sig"),
            )
            .expect("verify");
    }
}
