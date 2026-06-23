use std::env;

use anyhow::Context;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use ed25519_dalek::SigningKey;
use libp2p::identity::Keypair;
use libp2p::{multiaddr::Protocol, Multiaddr, PeerId};
use num_bigint::BigInt;

#[derive(Clone)]
pub struct NodeConfig {
    /// libp2p PeerID string (used as node_id in P2P payloads).
    pub peer_id: String,
    pub core_url: String,
    pub max_qubits: usize,
    pub compute_timeout_secs: u64,
    pub signing_key: SigningKey,
    pub bootstrap_peers: Vec<String>,
    pub p2p_listen_port: u16,
    pub http_port: u16,
    pub database_url: String,
    pub stake_amount: BigInt,
    pub orchestrator_peer_id: Option<PeerId>,
    /// Base64 Ed25519 public key of the trusted orchestrator (P2P dispatch + result trust).
    pub orchestrator_public_key: Option<String>,
}

impl NodeConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let core_url =
            env::var("WQC_CORE_URL").unwrap_or_else(|_| "http://localhost:3000".to_string());
        let max_qubits = env::var("WQC_MAX_QUBITS")
            .unwrap_or_else(|_| "30".to_string())
            .parse()
            .unwrap_or(30);
        let compute_timeout_secs = env::var("WQC_COMPUTE_TIMEOUT_SECS")
            .unwrap_or_else(|_| "300".to_string())
            .parse()
            .context("WQC_COMPUTE_TIMEOUT_SECS must be a valid positive integer")?;
        let signing_key = load_signing_key_from_env()?;
        let peer_id = libp2p_keypair_from_signing_key(&signing_key)?
            .public()
            .to_peer_id()
            .to_string();

        let bootstrap_peers: Vec<_> = env::var("WQC_ORCHESTRATOR_BOOTSTRAP")
            .context("WQC_ORCHESTRATOR_BOOTSTRAP is required (comma-separated libp2p multiaddrs)")?
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        if bootstrap_peers.is_empty() {
            anyhow::bail!("WQC_ORCHESTRATOR_BOOTSTRAP must include at least one multiaddr");
        }

        let p2p_listen_port = env::var("WQC_P2P_LISTEN_PORT")
            .unwrap_or_else(|_| "4002".to_string())
            .parse()
            .context("WQC_P2P_LISTEN_PORT must be a valid u16")?;

        let http_port = env::var("WQC_HTTP_PORT")
            .unwrap_or_else(|_| "8080".to_string())
            .parse()
            .context("WQC_HTTP_PORT must be a valid u16")?;

        let database_url =
            env::var("WQC_DATABASE_URL").unwrap_or_else(|_| "sqlite:wqc-node.db".to_string());

        let stake_wqc = env::var("WQC_NODE_STAKE_WQC").unwrap_or_else(|_| "0.05".to_string());
        let stake_amount = crate::domain::token::parse_wqc_to_planck(&stake_wqc)
            .with_context(|| format!("WQC_NODE_STAKE_WQC must be a valid WQC amount, got {stake_wqc:?}"))?;

        let orchestrator_peer_id = parse_orchestrator_peer_id(&bootstrap_peers);

        let orchestrator_public_key = Some(
            env::var("WQC_ORCHESTRATOR_PUBLIC_KEY")
                .context("WQC_ORCHESTRATOR_PUBLIC_KEY is required (base64 Ed25519 public key)")?
                .trim()
                .to_string(),
        )
        .filter(|s| !s.is_empty());

        tracing::info!(
            "Node Config Loaded: Max Qubits = {}, Compute Timeout = {}s",
            max_qubits,
            compute_timeout_secs
        );
        tracing::info!("Node libp2p PeerID: {}", peer_id);
        tracing::info!(
            "Node bid stake: {} WQC ({} pWQC)",
            stake_wqc,
            stake_amount
        );
        if let Some(peer) = orchestrator_peer_id {
            tracing::info!("Orchestrator libp2p PeerID: {}", peer);
        }

        Ok(Self {
            peer_id,
            core_url,
            max_qubits,
            compute_timeout_secs,
            signing_key,
            bootstrap_peers,
            p2p_listen_port,
            http_port,
            database_url,
            stake_amount,
            orchestrator_peer_id,
            orchestrator_public_key,
        })
    }
}

pub fn parse_orchestrator_peer_id(bootstrap_peers: &[String]) -> Option<PeerId> {
    for raw in bootstrap_peers {
        let Ok(addr) = raw.parse::<Multiaddr>() else {
            continue;
        };
        for protocol in addr.iter() {
            if let Protocol::P2p(peer_id) = protocol {
                return Some(peer_id);
            }
        }
    }
    None
}

fn load_signing_key_from_env() -> anyhow::Result<SigningKey> {
    let key_b64 = env::var("WQC_NODE_PRIVATE_KEY")
        .map_err(|_| anyhow::anyhow!("WQC_NODE_PRIVATE_KEY is required (base64 32-byte seed)"))?;
    let key_bytes = STANDARD
        .decode(key_b64.trim())
        .context("WQC_NODE_PRIVATE_KEY base64 decode failed")?;
    let key_array: [u8; 32] = key_bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("WQC_NODE_PRIVATE_KEY must decode to exactly 32 bytes"))?;
    Ok(SigningKey::from_bytes(&key_array))
}

pub fn libp2p_keypair_from_signing_key(signing_key: &SigningKey) -> anyhow::Result<Keypair> {
    Keypair::ed25519_from_bytes(signing_key.to_bytes())
        .map_err(|e| anyhow::anyhow!("failed to derive libp2p keypair: {:?}", e))
}
