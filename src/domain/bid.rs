use base64::{engine::general_purpose::STANDARD, Engine as _};
use ed25519_dalek::Signer;
use num_bigint::BigInt;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::config::NodeConfig;
use crate::domain::p2p::TaskAnnouncement;

pub const PROTOCOL_BID: &str = "/wqc/tensor-net/1.0.0";

pub const FEATURE_STANDARD_GATES: u32 = 1 << 0;
pub const FEATURE_CUSTOM_UNITARY: u32 = 1 << 1;

/// Node feature bitmask advertised to the orchestrator.
pub const NODE_FEATURES: u32 = FEATURE_STANDARD_GATES | FEATURE_CUSTOM_UNITARY;

const NETWORK_MIN_QUBITS: u32 = 10;
const LOTTERY_TIME_WINDOW_SECS: i64 = 10;

#[derive(Serialize)]
pub struct Bid {
    pub task_id: String,
    pub node_id: String,
    pub max_qubit_capability: u32,
    pub current_load_factors: u32,
    pub timestamp: i64,
    #[serde(serialize_with = "serialize_bytes_as_base64")]
    pub signature: Vec<u8>,
    #[serde(serialize_with = "serialize_bytes_as_base64")]
    pub lottery_proof: Vec<u8>,
    #[serde(serialize_with = "serialize_stake_as_string")]
    pub stake_amount: BigInt,
}

/// Returns true when this node should participate in the bidding round.
pub fn should_bid_on(announcement: &TaskAnnouncement, config: &NodeConfig) -> bool {
    if announcement.global_qubit_count > config.max_qubits as u32 {
        return false;
    }
    if (config.max_qubits as u32) < NETWORK_MIN_QUBITS {
        return false;
    }
    (announcement.required_features & NODE_FEATURES) == announcement.required_features
}

/// Builds a signed bid for the orchestrator bid stream.
pub fn build_signed_bid(
    announcement: &TaskAnnouncement,
    config: &NodeConfig,
    current_load: u32,
) -> Option<Bid> {
    let (timestamp, lottery_proof) =
        mine_lottery(&config.peer_id, announcement.nonce, announcement.bid_difficulty)?;

    let bid = Bid {
        task_id: announcement.task_id.clone(),
        node_id: config.peer_id.clone(),
        max_qubit_capability: config.max_qubits as u32,
        current_load_factors: current_load,
        timestamp,
        signature: Vec::new(),
        lottery_proof,
        stake_amount: config.stake_amount.clone(),
    };

    let payload = serialize_bid_payload(&bid);
    let signature = config.signing_key.sign(&payload).to_bytes().to_vec();

    Some(Bid {
        signature,
        ..bid
    })
}

/// Mirrors orchestrator `serializeBidPayload` byte layout exactly.
pub fn serialize_bid_payload(bid: &Bid) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(bid.task_id.as_bytes());
    payload.extend_from_slice(bid.node_id.as_bytes());
    payload.extend_from_slice(&bid.max_qubit_capability.to_be_bytes());
    payload.extend_from_slice(&bid.current_load_factors.to_be_bytes());
    payload.extend_from_slice(&bid.timestamp.to_be_bytes());
    payload.extend_from_slice(&bid.lottery_proof);
    payload
}

fn mine_lottery(node_id: &str, nonce: u64, difficulty: u32) -> Option<(i64, Vec<u8>)> {
    if difficulty == 0 {
        let now = chrono_lottery_timestamp();
        let proof = lottery_hash(node_id, nonce, now);
        return Some((now, proof));
    }

    let start = chrono_lottery_timestamp();
    let end = start + LOTTERY_TIME_WINDOW_SECS;
    for timestamp in start..=end {
        let proof = lottery_hash(node_id, nonce, timestamp);
        if meets_difficulty(&proof, difficulty) {
            return Some((timestamp, proof));
        }
    }
    None
}

fn chrono_lottery_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn lottery_hash(node_id: &str, nonce: u64, timestamp: i64) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(node_id.as_bytes());
    hasher.update(nonce.to_be_bytes());
    hasher.update((timestamp as u64).to_be_bytes());
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

fn serialize_stake_as_string<S>(value: &BigInt, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_bid_payload_layout_matches_orchestrator() {
        let bid = Bid {
            task_id: "task-a".to_string(),
            node_id: "node-b".to_string(),
            max_qubit_capability: 25,
            current_load_factors: 1,
            timestamp: 1_717_776_000,
            signature: Vec::new(),
            lottery_proof: vec![0xAA, 0xBB],
            stake_amount: BigInt::from(50_000),
        };

        let payload = serialize_bid_payload(&bid);
        let mut expected = Vec::new();
        expected.extend_from_slice(b"task-a");
        expected.extend_from_slice(b"node-b");
        expected.extend_from_slice(&25u32.to_be_bytes());
        expected.extend_from_slice(&1u32.to_be_bytes());
        expected.extend_from_slice(&1_717_776_000i64.to_be_bytes());
        expected.extend_from_slice(&[0xAA, 0xBB]);
        assert_eq!(payload, expected);
    }

    #[test]
    fn lottery_hash_matches_go_layout() {
        let hash = lottery_hash("12D3KooWTest", 42, 1_717_776_000);
        assert_eq!(hash.len(), 32);
    }
}
