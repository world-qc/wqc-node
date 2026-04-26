use std::collections::HashSet;
use std::env;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use ed25519_dalek::SigningKey;

#[derive(Clone)]
pub struct NodeConfig {
    pub core_url: String,
    pub max_qubits: usize,
    pub max_memory_kb: u32,
    pub signing_key: SigningKey,
    pub node_public_key_b64: String,
    pub allowed_orchestrator_pubkeys: HashSet<String>,
    pub dev_mode: bool,
}

impl NodeConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let core_url = env::var("WQC_CORE_URL").unwrap_or_else(|_| "http://localhost:3000".to_string());
        let max_qubits = env::var("WQC_MAX_QUBITS").unwrap_or_else(|_| "30".to_string()).parse().unwrap_or(30);
        let max_memory_kb = env::var("WQC_MAX_MEMORY_KB").unwrap_or_else(|_| "2097152".to_string()).parse().unwrap_or(2097152);
        let signing_key = load_signing_key_from_env()?;
        let node_public_key_b64 = STANDARD.encode(signing_key.verifying_key().to_bytes());
        let allowed_orchestrator_pubkeys = parse_pubkey_allowlist_env("WQC_ALLOWED_ORCHESTRATOR_PUBKEYS")?;

        let dev_mode = parse_bool_env("WQC_DEV_MODE");
        if !dev_mode && allowed_orchestrator_pubkeys.is_empty() {
            return Err(anyhow::anyhow!(
                "WQC_ALLOWED_ORCHESTRATOR_PUBKEYS is required unless WQC_DEV_MODE=true"
            ));
        }

        tracing::info!("Node Config Loaded: Max Qubits = {}, Max Memory = {} KB", max_qubits, max_memory_kb);
        tracing::info!("Node Public Key (base64): {}", node_public_key_b64);
        tracing::info!("Submit signature verification dev_mode={}", dev_mode);

        Ok(Self {
            core_url,
            max_qubits,
            max_memory_kb,
            signing_key,
            node_public_key_b64,
            allowed_orchestrator_pubkeys,
            dev_mode,
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

fn parse_pubkey_allowlist_env(var_name: &str) -> anyhow::Result<HashSet<String>> {
    let mut keys = HashSet::new();
    let raw = env::var(var_name).unwrap_or_default();
    for key in raw.split(',').map(|v| v.trim()).filter(|v| !v.is_empty()) {
        let decoded = STANDARD
            .decode(key)
            .map_err(|e| anyhow::anyhow!("{} invalid base64 key: {}", var_name, e))?;
        if decoded.len() != 32 {
            return Err(anyhow::anyhow!(
                "{} key must decode to 32 bytes",
                var_name
            ));
        }
        keys.insert(key.to_string());
    }
    Ok(keys)
}

fn parse_bool_env(var_name: &str) -> bool {
    matches!(
        env::var(var_name)
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}
