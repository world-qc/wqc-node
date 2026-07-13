use base64::{engine::general_purpose::STANDARD, Engine as _};
use ed25519_dalek::Signer;
use num_bigint::BigInt;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::config::NodeConfig;
use crate::domain::features::{self};
use crate::domain::geo::GeoInfo;
use crate::domain::operator::serialize_operator_bid_payload;
use crate::domain::p2p::TaskAnnouncement;

pub const PROTOCOL_BID: &str = "/wqc/tensor-net/1.0.0";

const NETWORK_MIN_QUBITS: u32 = 10;
const LOTTERY_TIME_WINDOW_SECS: i64 = 10;

#[derive(Serialize)]
pub struct Bid {
    pub task_id: String,
    pub node_id: String,
    pub max_qubit_capability: u32,
    pub current_load_factors: u32,
    pub timestamp: i64,
    pub lottery_attempt: u64,
    #[serde(serialize_with = "serialize_bytes_as_base64")]
    pub signature: Vec<u8>,
    #[serde(serialize_with = "serialize_bytes_as_base64")]
    pub lottery_proof: Vec<u8>,
    #[serde(serialize_with = "serialize_stake_as_string")]
    pub stake_amount: BigInt,
    pub supported_features: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<GeoInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operator_id: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_optional_bytes_as_base64"
    )]
    pub operator_sig: Option<Vec<u8>>,
    /// Unsigned health snapshot for orchestrator Prometheus aggregation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics_summary: Option<crate::infra::metrics::MetricsSummary>,
}

/// Returns true when this node should participate in the bidding round.
///
/// `supported_features` is derived from wqc-core `GET /gates` at node startup.
/// Parent tasks may advertise more qubits than a single node can execute; the orchestrator
/// slices branches down to each node's `max_qubit_capability` before dispatch.
pub fn should_bid_on(
    announcement: &TaskAnnouncement,
    config: &NodeConfig,
    supported_features: u32,
) -> bool {
    if (config.max_qubits as u32) < NETWORK_MIN_QUBITS {
        return false;
    }
    if supported_features == 0 {
        return false;
    }
    features::supports_required_features(supported_features, announcement.required_features)
}

/// Builds a signed bid for the orchestrator bid stream.
pub fn build_signed_bid(
    announcement: &TaskAnnouncement,
    config: &NodeConfig,
    current_load: u32,
    supported_features: u32,
    location: Option<GeoInfo>,
    metrics_summary: Option<crate::infra::metrics::MetricsSummary>,
) -> Option<Bid> {
    let operator_id = config.operator_id.clone()?;
    let operator_signing_key = config.operator_signing_key.as_ref()?;

    let (timestamp, lottery_attempt, lottery_proof) = mine_lottery(
        &config.peer_id,
        announcement.nonce,
        announcement.bid_difficulty,
    )?;

    let bid = Bid {
        task_id: announcement.task_id.clone(),
        node_id: config.peer_id.clone(),
        max_qubit_capability: config.max_qubits as u32,
        current_load_factors: current_load,
        timestamp,
        lottery_attempt,
        signature: Vec::new(),
        lottery_proof,
        stake_amount: config.stake_amount.clone(),
        supported_features,
        location,
        operator_id: Some(operator_id.clone()),
        operator_sig: None,
        metrics_summary,
    };

    let payload = serialize_bid_payload(&bid);
    let signature = config.signing_key.sign(&payload).to_bytes().to_vec();

    let operator_payload =
        serialize_operator_bid_payload(&operator_id, &bid.node_id, &bid.task_id, &bid.stake_amount);
    let operator_sig = operator_signing_key
        .sign(&operator_payload)
        .to_bytes()
        .to_vec();

    Some(Bid {
        signature,
        operator_sig: Some(operator_sig),
        ..bid
    })
}

/// Mirrors orchestrator `bid.SerializeBidPayload` byte layout exactly.
///
/// All fixed-width integers are big-endian (network byte order).
pub fn serialize_bid_payload(bid: &Bid) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(bid.task_id.as_bytes());
    payload.extend_from_slice(bid.node_id.as_bytes());
    payload.extend_from_slice(&bid.max_qubit_capability.to_be_bytes());
    payload.extend_from_slice(&bid.current_load_factors.to_be_bytes());
    payload.extend_from_slice(&bid.timestamp.to_be_bytes());
    payload.extend_from_slice(&bid.lottery_attempt.to_be_bytes());
    payload.extend_from_slice(&bid.lottery_proof);
    payload.extend_from_slice(&bid.supported_features.to_be_bytes());
    payload
}

fn mine_lottery(node_id: &str, nonce: u64, difficulty: u32) -> Option<(i64, u64, Vec<u8>)> {
    let timestamp = chrono_lottery_timestamp();

    if difficulty == 0 {
        let proof = lottery_hash(node_id, nonce, timestamp, 0);
        return Some((timestamp, 0, proof));
    }

    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(LOTTERY_TIME_WINDOW_SECS as u64);
    let mut attempt: u64 = 0;
    while std::time::Instant::now() < deadline {
        let proof = lottery_hash(node_id, nonce, timestamp, attempt);
        if meets_difficulty(&proof, difficulty) {
            return Some((timestamp, attempt, proof));
        }
        attempt = attempt.wrapping_add(1);
    }
    None
}

fn chrono_lottery_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// SHA256(node_id || nonce_be || timestamp_as_u64_be || attempt_be).
fn lottery_hash(node_id: &str, nonce: u64, timestamp: i64, attempt: u64) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(node_id.as_bytes());
    hasher.update(nonce.to_be_bytes());
    hasher.update((timestamp as u64).to_be_bytes());
    hasher.update(attempt.to_be_bytes());
    hasher.finalize().to_vec()
}

fn meets_difficulty(hash: &[u8], difficulty: u32) -> bool {
    if difficulty == 0 {
        return true;
    }
    if hash.len() < difficulty as usize {
        return false;
    }
    hash[..difficulty as usize].iter().all(|&b| b == 0)
}

fn serialize_bytes_as_base64<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&STANDARD.encode(bytes))
}

fn serialize_optional_bytes_as_base64<S>(
    bytes: &Option<Vec<u8>>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match bytes {
        Some(value) => serializer.serialize_str(&STANDARD.encode(value)),
        None => serializer.serialize_none(),
    }
}

fn serialize_stake_as_string<S>(value: &BigInt, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::NodeConfig;
    use crate::domain::features::{FEATURE_CUSTOM_UNITARY, FEATURE_STANDARD_GATES};
    use crate::domain::p2p::TaskAnnouncement;
    use ed25519_dalek::SigningKey;

    fn test_config(max_qubits: usize) -> NodeConfig {
        NodeConfig {
            peer_id: "test".to_string(),
            core_url: String::new(),
            max_qubits,
            max_memory_gib: 0.0,
            compute_timeout_secs: 300,
            signing_key: SigningKey::from_bytes(&[0u8; 32]),
            bootstrap_urls: vec![],
            bootstrap_source_url: None,
            bootstrap_peers: vec![],
            p2p_listen_port: 0,
            http_port: 0,
            database_url: String::new(),
            stake_amount: BigInt::from(0),
            orchestrator_peer_id: None,
            orchestrator_public_key: None,
            operator_id: None,
            operator_signing_key: None,
        }
    }

    #[test]
    fn should_bid_when_parent_qubits_exceed_node_cap() {
        let announcement = TaskAnnouncement {
            task_id: "task-30".to_string(),
            global_qubit_count: 30,
            required_features: FEATURE_STANDARD_GATES,
            bid_difficulty: 0,
            required_votes: 1,
            nonce: 1,
        };
        assert!(should_bid_on(
            &announcement,
            &test_config(28),
            FEATURE_STANDARD_GATES
        ));
    }

    #[test]
    fn should_not_bid_when_features_unsupported() {
        let announcement = TaskAnnouncement {
            task_id: "task-30".to_string(),
            global_qubit_count: 30,
            required_features: FEATURE_STANDARD_GATES | FEATURE_CUSTOM_UNITARY | (1 << 5),
            bid_difficulty: 0,
            required_votes: 1,
            nonce: 1,
        };
        assert!(!should_bid_on(
            &announcement,
            &test_config(28),
            FEATURE_STANDARD_GATES
        ));
    }

    #[test]
    fn should_not_bid_when_core_reports_no_features() {
        let announcement = TaskAnnouncement {
            task_id: "task-30".to_string(),
            global_qubit_count: 30,
            required_features: FEATURE_STANDARD_GATES,
            bid_difficulty: 0,
            required_votes: 1,
            nonce: 1,
        };
        assert!(!should_bid_on(&announcement, &test_config(28), 0));
    }

    #[test]
    fn serialize_bid_payload_layout_matches_orchestrator() {
        let supported = FEATURE_STANDARD_GATES | FEATURE_CUSTOM_UNITARY;
        let bid = Bid {
            task_id: "task-a".to_string(),
            node_id: "node-b".to_string(),
            max_qubit_capability: 25,
            current_load_factors: 1,
            timestamp: 1_717_776_000,
            lottery_attempt: 7,
            signature: Vec::new(),
            lottery_proof: vec![0xAA, 0xBB],
            stake_amount: BigInt::from(50_000),
            supported_features: supported,
            location: None,
            operator_id: None,
            operator_sig: None,
            metrics_summary: None,
        };

        let payload = serialize_bid_payload(&bid);
        let mut expected = Vec::new();
        expected.extend_from_slice(b"task-a");
        expected.extend_from_slice(b"node-b");
        expected.extend_from_slice(&25u32.to_be_bytes());
        expected.extend_from_slice(&1u32.to_be_bytes());
        expected.extend_from_slice(&1_717_776_000i64.to_be_bytes());
        expected.extend_from_slice(&7u64.to_be_bytes());
        expected.extend_from_slice(&[0xAA, 0xBB]);
        expected.extend_from_slice(&supported.to_be_bytes());
        assert_eq!(payload, expected);
    }

    #[test]
    fn lottery_hash_matches_go_golden_vector() {
        let hash = lottery_hash("12D3KooWTest", 42, 1_717_776_000, 0);
        assert_eq!(
            hash,
            [
                0x6e, 0x53, 0xa4, 0x11, 0x24, 0xed, 0x5c, 0xa6, 0xbc, 0x0c, 0x82, 0xb4, 0xd9, 0x27,
                0x12, 0x4a, 0x7f, 0xb6, 0xd0, 0x8f, 0xa4, 0x23, 0xb3, 0xe5, 0x05, 0x08, 0x9c, 0x8a,
                0x8f, 0xa6, 0x65, 0xb2,
            ]
        );
    }
}
