use std::env;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use ed25519_dalek::SigningKey;

#[derive(Clone)]
pub struct NodeConfig {
    pub node_host: String,
    pub node_port: usize,
    pub core_url: String,
    pub max_qubits: usize,
    pub max_memory_cost_kb: u32,
    pub min_difficulty: u32,
    pub max_difficulty: u32,
    pub signing_key: SigningKey,
    pub node_public_key_b64: String,
    pub orchestrator_urls: Vec<String>,
    pub database_url: String,
}

impl NodeConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let node_host = env::var("WQC_NODE_HOST").unwrap_or_else(|_| "wqc-node".to_string());
        let node_port = env::var("WQC_NODE_POrt").unwrap_or_else(|_| "8080".to_string()).parse().unwrap_or(8080);
        let core_url = env::var("WQC_CORE_URL").unwrap_or_else(|_| "http://localhost:3000".to_string());
        let max_qubits = env::var("WQC_MAX_QUBITS").unwrap_or_else(|_| "30".to_string()).parse().unwrap_or(30);
        let max_memory_cost_kb = env::var("WQC_MAX_MEMORY_COST_KB").unwrap_or_else(|_| "2097152".to_string()).parse().unwrap_or(2097152);
        let min_difficulty = env::var("WQC_MIN_DIFFICULTY").unwrap_or_else(|_| "10".to_string()).parse().unwrap_or(10);
        let max_difficulty = env::var("WQC_MAX_DIFFICULTY").unwrap_or_else(|_| "32".to_string()).parse().unwrap_or(32);
        let signing_key = load_signing_key_from_env()?;
        let node_public_key_b64 = STANDARD.encode(signing_key.verifying_key().to_bytes());
        let orchestrator_urls: Vec<_> = env::var("WQC_ORCHESTRATOR_URLS")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        if orchestrator_urls.is_empty() {
            return Err(anyhow::anyhow!(
                "WQC_ORCHESTRATOR_URLS is required"
            ));
        }

        let database_url = env::var("WQC_DATABASE_URL").unwrap_or_else(|_| "salite:wqc-node.db".to_string());

        tracing::info!("Node Config Loaded: Max Qubits = {}, Max Memory = {} KB", max_qubits, max_memory_cost_kb);
        tracing::info!("Node Public Key (base64): {}", node_public_key_b64);

        Ok(Self {
            node_host,
            node_port,
            core_url,
            max_qubits,
            max_memory_cost_kb,
            min_difficulty,
            max_difficulty,
            signing_key,
            node_public_key_b64,
            orchestrator_urls,
            database_url,
        })
    }
}

fn load_signing_key_from_env() -> anyhow::Result<SigningKey> {
    let key_b64 = env::var("WQC_NODE_PRIVATE_KEY")
        .map_err(|_| anyhow::anyhow!("WQC_NODE_PRIVATE_KEY is required (base64 32-byte seed)"))?;
    let key_bytes = STANDARD
        .decode(key_b64.trim())
        .map_err(|e| anyhow::anyhow!("WQC_NODE_PRIVATE_KEY base64 decode failed: {}", e))?;
    let key_array: [u8; 32] = key_bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("WQC_NODE_PRIVATE_KEY must decode to exactly 32 bytes"))?;
    Ok(SigningKey::from_bytes(&key_array))
}
