use std::env;

use anyhow::Context;
use crate::memory_budget::resolve_max_qubits_from_memory_gb;
use sysinfo::{MemoryRefreshKind, RefreshKind, System};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use ed25519_dalek::SigningKey;
use libp2p::identity::Keypair;
use libp2p::PeerId;
use num_bigint::BigInt;

use crate::infra::orchestrator::OrchestratorBootstrap;

#[derive(Clone)]
pub struct NodeConfig {
    /// libp2p PeerID string (used as node_id in P2P payloads).
    pub peer_id: String,
    pub core_url: String,
    /// Derived from `WQC_MAX_MEMORY_GB` (dense `2^n × 16` envelope); advertised in bids.
    pub max_qubits: usize,
    /// Effective WQC memory budget (GiB) after host reserve cap.
    pub max_memory_gib: f64,
    pub compute_timeout_secs: u64,
    pub signing_key: SigningKey,
    /// Comma-separated bootstrap endpoint URLs from env (full path, failover order).
    pub bootstrap_urls: Vec<String>,
    /// Bootstrap URL that successfully returned P2P discovery info.
    pub bootstrap_source_url: Option<String>,
    pub bootstrap_peers: Vec<String>,
    pub p2p_listen_port: u16,
    pub http_port: u16,
    pub database_url: String,
    pub stake_amount: BigInt,
    pub orchestrator_peer_id: Option<PeerId>,
    /// Base64 Ed25519 public key of the trusted orchestrator (P2P dispatch + result trust).
    pub orchestrator_public_key: Option<String>,
    /// Economic operator identity (sha256 hex of derived operator pubkey).
    pub operator_id: Option<String>,
    /// Ed25519 key derived from `WQC_TESTNET_NODE_KEY` for operator bid signatures.
    pub operator_signing_key: Option<SigningKey>,
}

impl NodeConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let core_url =
            env::var("WQC_CORE_URL").unwrap_or_else(|_| "http://localhost:3000".to_string());
        let requested_memory_gib = env::var("WQC_MAX_MEMORY_GB")
            .unwrap_or_else(|_| "16".to_string())
            .parse::<f64>()
            .context("WQC_MAX_MEMORY_GB must be a valid number")?;
        let mut sys = System::new_with_specifics(
            RefreshKind::new().with_memory(MemoryRefreshKind::everything()),
        );
        sys.refresh_memory();
        let total_physical_bytes = sys.total_memory();
        let (max_qubits, max_memory_gib) =
            resolve_max_qubits_from_memory_gb(requested_memory_gib, total_physical_bytes);
        let compute_timeout_secs = env::var("WQC_COMPUTE_TIMEOUT_SECS")
            .unwrap_or_else(|_| "300".to_string())
            .parse()
            .context("WQC_COMPUTE_TIMEOUT_SECS must be a valid positive integer")?;
        let signing_key = load_signing_key_from_env()?;
        let peer_id = libp2p_keypair_from_signing_key(&signing_key)?
            .public()
            .to_peer_id()
            .to_string();

        let bootstrap_urls: Vec<_> = env::var("WQC_BOOTSTRAP_URLS")
            .context("WQC_BOOTSTRAP_URLS is required (comma-separated full bootstrap HTTP(S) URLs)")?
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        if bootstrap_urls.is_empty() {
            anyhow::bail!("WQC_BOOTSTRAP_URLS must include at least one URL");
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

        tracing::info!(
            "Node Config Loaded: WQC memory budget = {:.2} GiB (requested {:.2} GiB, host total {} KiB) → max_qubits = {}, compute timeout = {}s",
            max_memory_gib,
            requested_memory_gib,
            total_physical_bytes / 1024,
            max_qubits,
            compute_timeout_secs
        );
        tracing::info!("Node libp2p PeerID: {}", peer_id);
        tracing::info!(
            "Node bid stake: {} WQC ({} pWQC)",
            stake_wqc,
            stake_amount
        );

        let (operator_id, operator_signing_key) =
            match env::var("WQC_TESTNET_NODE_KEY") {
                Ok(node_key) if !node_key.trim().is_empty() => {
                    let (operator_id, key) =
                        crate::domain::operator::derive_operator_keypair(node_key.trim())?;
                    tracing::info!("Operator ID loaded from WQC_TESTNET_NODE_KEY: {}", operator_id);
                    (Some(operator_id), Some(key))
                }
                _ => {
                    tracing::warn!(
                        "WQC_TESTNET_NODE_KEY is not set; bids will be rejected (operator signature required)"
                    );
                    (None, None)
                }
            };

        Ok(Self {
            peer_id,
            core_url,
            max_qubits,
            max_memory_gib,
            compute_timeout_secs,
            signing_key,
            bootstrap_urls,
            bootstrap_source_url: None,
            bootstrap_peers: Vec::new(),
            p2p_listen_port,
            http_port,
            database_url,
            stake_amount,
            orchestrator_peer_id: None,
            orchestrator_public_key: None,
            operator_id,
            operator_signing_key,
        })
    }

    pub fn apply_orchestrator_bootstrap(
        &mut self,
        bootstrap: OrchestratorBootstrap,
    ) -> anyhow::Result<()> {
        if bootstrap.multiaddrs.is_empty() {
            anyhow::bail!("orchestrator bootstrap returned no multiaddrs");
        }
        self.bootstrap_source_url = Some(bootstrap.source_url);
        self.orchestrator_peer_id = Some(bootstrap.peer_id);
        self.orchestrator_public_key = Some(bootstrap.public_key_b64);
        self.bootstrap_peers = bootstrap.multiaddrs;
        tracing::info!("Orchestrator libp2p PeerID: {}", bootstrap.peer_id);
        Ok(())
    }
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
