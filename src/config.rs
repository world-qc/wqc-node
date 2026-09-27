use std::env;

use crate::domain::models::CoreSystemInfo;
use crate::memory_budget::{
    effective_memory_bytes, resolve_max_qubits_from_memory_gb,
    resolve_max_qubits_with_optional_vram, MemoryCapSource,
};
use anyhow::Context;
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
    /// Derived from memory budget (dense `2^n × 16` envelope); advertised in bids.
    pub max_qubits: usize,
    /// Effective WQC memory budget (GiB) after host / optional VRAM caps.
    pub max_memory_gib: f64,
    /// Operator request from `WQC_MAX_MEMORY_GB` (`None` = unset → host−reserve).
    pub requested_memory_gib: Option<f64>,
    /// Host physical RAM at config load (bytes).
    pub host_total_memory_bytes: u64,
    /// Which envelope limited `max_memory_gib` (`ram` or `vram`).
    pub memory_cap_source: MemoryCapSource,
    /// Adapter VRAM budget from core `/sysinfo` when WebGPU (GiB), if known.
    pub vram_budget_gib: Option<f64>,
    pub compute_timeout_secs: u64,
    /// Wall-clock budget for deferred `POST /leaf_pcs` (default 7200s).
    pub pcs_timeout_secs: u64,
    pub signing_key: SigningKey,
    /// Comma-separated bootstrap endpoint URLs from env (full path, failover order).
    pub bootstrap_urls: Vec<String>,
    /// Bootstrap URL that successfully returned P2P discovery info.
    pub bootstrap_source_url: Option<String>,
    pub bootstrap_peers: Vec<String>,
    pub p2p_listen_port: u16,
    /// libp2p idle connection timeout in seconds (`WQC_P2P_IDLE_TIMEOUT_SECS`, default 60).
    pub p2p_idle_timeout_secs: u64,
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
        let requested_memory_gib = parse_requested_memory_gib()?;
        let mut sys = System::new_with_specifics(
            RefreshKind::new().with_memory(MemoryRefreshKind::everything()),
        );
        sys.refresh_memory();
        let host_total_memory_bytes = sys.total_memory();
        let (max_qubits, max_memory_gib) =
            resolve_max_qubits_from_memory_gb(requested_memory_gib, host_total_memory_bytes);
        let compute_timeout_secs = env::var("WQC_COMPUTE_TIMEOUT_SECS")
            .unwrap_or_else(|_| "300".to_string())
            .parse()
            .context("WQC_COMPUTE_TIMEOUT_SECS must be a valid positive integer")?;
        let pcs_timeout_secs = env::var("WQC_PCS_TIMEOUT_SECS")
            .unwrap_or_else(|_| "7200".to_string())
            .parse()
            .context("WQC_PCS_TIMEOUT_SECS must be a valid positive integer")?;
        let signing_key = load_signing_key_from_env()?;
        let peer_id = libp2p_keypair_from_signing_key(&signing_key)?
            .public()
            .to_peer_id()
            .to_string();

        let bootstrap_urls: Vec<_> = env::var("WQC_BOOTSTRAP_URLS")
            .context(
                "WQC_BOOTSTRAP_URLS is required (comma-separated full bootstrap HTTP(S) URLs)",
            )?
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

        let p2p_idle_timeout_secs = env::var("WQC_P2P_IDLE_TIMEOUT_SECS")
            .unwrap_or_else(|_| "60".to_string())
            .parse()
            .context("WQC_P2P_IDLE_TIMEOUT_SECS must be a valid positive integer")?;

        let http_port = env::var("WQC_HTTP_PORT")
            .unwrap_or_else(|_| "8080".to_string())
            .parse()
            .context("WQC_HTTP_PORT must be a valid u16")?;

        let database_url =
            env::var("WQC_DATABASE_URL").unwrap_or_else(|_| "sqlite:wqc-node.db".to_string());

        let stake_wqc = env::var("WQC_NODE_STAKE_WQC").unwrap_or_else(|_| "0.05".to_string());
        let stake_amount =
            crate::domain::token::parse_wqc_to_planck(&stake_wqc).with_context(|| {
                format!("WQC_NODE_STAKE_WQC must be a valid WQC amount, got {stake_wqc:?}")
            })?;

        let requested_log = requested_memory_gib
            .map(|g| format!("{g:.2} GiB"))
            .unwrap_or_else(|| "unset (host−reserve)".into());
        tracing::info!(
            "Node Config Loaded: WQC memory budget = {:.2} GiB (requested {}, host total {} KiB, cap={}) → max_qubits = {}, compute timeout = {}s",
            max_memory_gib,
            requested_log,
            host_total_memory_bytes / 1024,
            MemoryCapSource::Ram.as_str(),
            max_qubits,
            compute_timeout_secs,
        );
        tracing::info!("Node libp2p PeerID: {}", peer_id);
        tracing::info!("Node bid stake: {} WQC ({} pWQC)", stake_wqc, stake_amount);

        let (operator_id, operator_signing_key) = match env::var("WQC_TESTNET_NODE_KEY") {
            Ok(node_key) if !node_key.trim().is_empty() => {
                let (operator_id, key) =
                    crate::domain::operator::derive_operator_keypair(node_key.trim())?;
                tracing::info!(
                    "Operator ID loaded from WQC_TESTNET_NODE_KEY: {}",
                    operator_id
                );
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
            requested_memory_gib,
            host_total_memory_bytes,
            memory_cap_source: MemoryCapSource::Ram,
            vram_budget_gib: None,
            compute_timeout_secs,
            pcs_timeout_secs,
            signing_key,
            bootstrap_urls,
            bootstrap_source_url: None,
            bootstrap_peers: Vec::new(),
            p2p_listen_port,
            p2p_idle_timeout_secs,
            http_port,
            database_url,
            stake_amount,
            orchestrator_peer_id: None,
            orchestrator_public_key: None,
            operator_id,
            operator_signing_key,
        })
    }

    /// After core `/sysinfo`: if WebGPU is active, take `min(RAM, VRAM−reserve)` for bids.
    pub fn apply_core_sysinfo(&mut self, info: &CoreSystemInfo) {
        let ram_bytes =
            effective_memory_bytes(self.requested_memory_gib, self.host_total_memory_bytes);

        let vram_raw = if info.tn_backend_active.eq_ignore_ascii_case("webgpu") {
            info.vram_budget_bytes.filter(|&b| b > 0)
        } else {
            None
        };

        self.vram_budget_gib = vram_raw.map(|b| b as f64 / (1024.0 * 1024.0 * 1024.0));

        let (max_qubits, max_memory_gib, source) =
            resolve_max_qubits_with_optional_vram(ram_bytes, vram_raw);

        if source != self.memory_cap_source
            || (max_memory_gib - self.max_memory_gib).abs() > 0.01
            || max_qubits != self.max_qubits
        {
            tracing::info!(
                "Memory budget after core sysinfo: {:.2} GiB → max_qubits = {} (cap={}, tn_backend={}, vram_budget_gib={:?})",
                max_memory_gib,
                max_qubits,
                source.as_str(),
                info.tn_backend_active,
                self.vram_budget_gib,
            );
        }

        self.max_qubits = max_qubits;
        self.max_memory_gib = max_memory_gib;
        self.memory_cap_source = source;
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

/// `None` = env unset/empty → host−reserve. `Some` = explicit GiB request.
fn parse_requested_memory_gib() -> anyhow::Result<Option<f64>> {
    match env::var("WQC_MAX_MEMORY_GB") {
        Err(_) => Ok(None),
        Ok(s) if s.trim().is_empty() => Ok(None),
        Ok(s) => s
            .trim()
            .parse::<f64>()
            .map(Some)
            .context("WQC_MAX_MEMORY_GB must be a valid number"),
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
