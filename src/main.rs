mod models;
mod core_client;

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use core_client::WqcCoreClient;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use models::{ComputeRequest, Gate, WebhookPayload};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::env;
use std::sync::{atomic::{AtomicUsize, Ordering}, Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use uuid::Uuid;

struct NodeConfig {
    max_qubits: usize,
    max_memory_kb: u32,
    signing_key: SigningKey,
    node_public_key_b64: String,
    allowed_orchestrator_pubkeys: HashSet<String>,
    dev_mode: bool,
}

impl NodeConfig {
    fn from_env() -> anyhow::Result<Self> {
        let signing_key = load_signing_key_from_env()?;
        let node_public_key_b64 = STANDARD.encode(signing_key.verifying_key().to_bytes());
        let dev_mode = parse_bool_env("WQC_DEV_MODE");
        let allowed_orchestrator_pubkeys =
            parse_pubkey_allowlist_env("WQC_ALLOWED_ORCHESTRATOR_PUBKEYS")?;
        if !dev_mode && allowed_orchestrator_pubkeys.is_empty() {
            return Err(anyhow::anyhow!(
                "WQC_ALLOWED_ORCHESTRATOR_PUBKEYS is required unless WQC_DEV_MODE=true"
            ));
        }

        Ok(Self {
            // Default to 30 qubits if not set
            max_qubits: env::var("WQC_MAX_QUBITS")
                .unwrap_or_else(|_| "30".to_string())
                .parse().unwrap_or(30),
            // Default to 2GB (2,097,152 KB) if not set
            max_memory_kb: env::var("WQC_MAX_MEMORY_KB")
                .unwrap_or_else(|_| "2097152".to_string())
                .parse().unwrap_or(2097152),
            signing_key,
            node_public_key_b64,
            allowed_orchestrator_pubkeys,
            dev_mode,
        })
    }
}

// Global state shared across API handlers
struct AppState {
    config: NodeConfig,
    task_sender: mpsc::Sender<ComputeRequest>,
    // Counter for tasks currently in the queue or being processed
    pending_tasks: AtomicUsize,
    supported_gates: Vec<String>,
    seen_submit_nonces: Mutex<HashMap<String, i64>>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    // Load the configuration from environment variables here!
    let config = NodeConfig::from_env()?;
    tracing::info!(
        "Node Config Loaded: Max Qubits = {}, Max Memory = {} KB",
        config.max_qubits, config.max_memory_kb
    );
    tracing::info!("Node Public Key (base64): {}", config.node_public_key_b64);
    tracing::info!("Submit signature verification dev_mode={}", config.dev_mode);

    let core_url = env::var("WQC_CORE_URL").unwrap_or_else(|_| "http://localhost:3000".to_string());

    // Fetch Supported Gates from wqc-core
    let client = reqwest::Client::new();
    let supported_gates = match client.get(format!("{}/gates", core_url)).send().await {
        Ok(resp) => resp.json::<Vec<String>>().await.unwrap_or_default(),
        Err(e) => {
            tracing::error!("Failed to fetch gates from wqc-core: {}", e);
            return Err(anyhow::anyhow!("Core connection failed"));
        }
    };

    tracing::info!("Synchronized with wqc-core. Supported gates: {:?}", supported_gates);

    let (tx, mut rx) = mpsc::channel::<ComputeRequest>(100);

    let shared_state = Arc::new(AppState {
        task_sender: tx,
        pending_tasks: AtomicUsize::new(0),
        config,
        supported_gates,
        seen_submit_nonces: Mutex::new(HashMap::new()),
    });

    // --- Worker Task ---
    let worker_state = Arc::clone(&shared_state);
    let client = Arc::new(WqcCoreClient::new(&core_url));

    tokio::spawn(async move {
        while let Some(task) = rx.recv().await {
            let task_id = task.task_id.clone();
            // Capture the webhook URL from the task request
            let callback_url = task.webhook_url.clone();

            tracing::info!("Worker: Starting Task {}", task_id);

            // Execute computation
            let result = client.dispatch_task(task).await;

            // Prepare Webhook payload based on result
            let payload = match result {
                Ok(res) => WebhookPayload {
                    task_id: res.task_id,
                    status: res.status, // "success"
                    state_vector: Some(res.state_vector),
                    proof: Some(res.proof),
                    error: None,
                },
                Err(e) => WebhookPayload {
                    task_id: task_id.clone(),
                    status: "failed".to_string(),
                    state_vector: None,
                    proof: None,
                    error: Some(e.to_string()),
                },
            };

            // Send Webhook only if callback_url is provided
            if let Some(url) = callback_url {
                let http_client = reqwest::Client::new();
                let pid = payload.task_id.clone();
                let body = match serde_json::to_vec(&payload) {
                    Ok(body) => body,
                    Err(e) => {
                        tracing::error!("Webhook: Failed to serialize payload for task {}: {}", pid, e);
                        worker_state.pending_tasks.fetch_sub(1, Ordering::SeqCst);
                        continue;
                    }
                };
                let timestamp = current_unix_timestamp();
                let nonce = Uuid::new_v4().to_string();
                let signature = create_webhook_signature(
                    &worker_state.config.signing_key,
                    &body,
                    timestamp,
                    &nonce,
                );
                match http_client
                    .post(&url)
                    .header("content-type", "application/json")
                    .header("x-wqc-node-publickey", &worker_state.config.node_public_key_b64)
                    .header("x-wqc-timestamp", timestamp.to_string())
                    .header("x-wqc-nonce", &nonce)
                    .header("x-wqc-signature", signature)
                    .body(body)
                    .send()
                    .await
                {
                    Ok(_) => tracing::info!("Webhook: Notified {} for task {}", url, pid),
                    Err(e) => tracing::error!("Webhook: Failed to notify {}: {}", url, e),
                }
            } else {
                tracing::warn!("Worker: Task {} finished but no webhook_url was provided.", task_id);
            }

            // Decrement pending counter
            worker_state.pending_tasks.fetch_sub(1, Ordering::SeqCst);
        }
    });

    // --- Router ---
    let app = Router::new()
        .route("/submit", post(submit_task))
        .route("/status", get(get_status)) // New endpoint
        .with_state(shared_state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    axum::serve(listener, app).await?;
    Ok(())
}

// --- Handlers ---

async fn submit_task(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, Json<serde_json::Value>) {
    if let Err(reason) = verify_submit_signature(&state, &headers, &body) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": reason
            })),
        );
    }

    let payload: ComputeRequest = match serde_json::from_slice(&body) {
        Ok(payload) => payload,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!("Invalid JSON payload: {}", e)
                })),
            );
        }
    };

    tracing::info!("API: Validating task submission: {}", payload.task_id);

    // 1. Node Capacity Validation (Static Config)
    if payload.qubit_count > state.config.max_qubits {
        tracing::warn!("Rejecting: qubit_count exceeds node limit");
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "qubit_count exceeds node limit" })),
        );
    }

    if payload.memory_cost_kb > state.config.max_memory_kb {
        tracing::warn!("Rejecting: memory_cost_kb exceeds node limit");
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "memory_cost_kb exceeds node limit" })),
        );
    }

    // 2. Circuit Logic Validation (Dynamic Sync Data)
    // Pass the gates synced from wqc-core and the request's qubit_count
    if let Err(err_msg) = validate_circuit(
        &payload.circuit,
        &state.supported_gates,
        payload.qubit_count
    ) {
        tracing::warn!("Rejecting: Circuit validation failed: {}", err_msg);
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": err_msg })),
        );
    }

    // 3. Queue the task
    state.pending_tasks.fetch_add(1, Ordering::SeqCst);
    if let Err(e) = state.task_sender.send(payload).await {
        tracing::error!("API: Failed to queue task: {}", e);
        state.pending_tasks.fetch_sub(1, Ordering::SeqCst);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "Failed to queue task" })),
        );
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "accepted",
            "message": "Task is valid and has been queued."
        })),
    )
}

async fn get_status(
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let count = state.pending_tasks.load(Ordering::SeqCst);

    Json(serde_json::json!({
        "pending_tasks_count": count,
        "is_busy": count > 0,
        "node_version": "0.1.0"
    }))
}

// Helper function to validate the circuit before queuing
fn validate_circuit(
    circuit: &[Gate],
    supported_gates: &[String],
    max_qubits: usize
) -> Result<(), String> {
    for (idx, gate) in circuit.iter().enumerate() {
        // 1. Check if the gate type is supported by the current wqc-core
        if !supported_gates.contains(&gate.r#type) {
            return Err(format!(
                "Gate '{}' at index {} is not supported by this node",
                gate.r#type, idx
            ));
        }

        // 2. Check if qubit indices are within the allocated range
        // Assuming params is a JSON array of qubit indices for gates like CNOT
        if let Some(params_array) = gate.params.as_array() {
            for param in params_array {
                if let Some(qubit_idx) = param.as_u64() {
                    if qubit_idx as usize >= max_qubits {
                        return Err(format!(
                            "Qubit index {} at gate {} exceeds qubit_count {}",
                            qubit_idx, idx, max_qubits
                        ));
                    }
                }
            }
        } else if let Some(qubit_idx) = gate.params.as_u64() {
            // Case for single-qubit gates where params might be a single integer
            if qubit_idx as usize >= max_qubits {
                return Err(format!(
                    "Qubit index {} at gate {} exceeds qubit_count {}",
                    qubit_idx, idx, max_qubits
                ));
            }
        }
    }
    Ok(())
}

fn create_webhook_signature(
    signing_key: &SigningKey,
    body: &[u8],
    timestamp: i64,
    nonce: &str,
) -> String {
    let message = build_signature_message(body, timestamp, nonce);
    let signature = signing_key.sign(message.as_bytes());
    STANDARD.encode(signature.to_bytes())
}

fn build_signature_message(body: &[u8], timestamp: i64, nonce: &str) -> String {
    build_signature_message_with_prefix("WQC-WEBHOOK-V1", body, timestamp, nonce)
}

fn build_request_signature_message(body: &[u8], timestamp: i64, nonce: &str) -> String {
    build_signature_message_with_prefix("WQC-REQUEST-V1", body, timestamp, nonce)
}

fn build_signature_message_with_prefix(
    prefix: &str,
    body: &[u8],
    timestamp: i64,
    nonce: &str,
) -> String {
    let body_hash = Sha256::digest(body);
    let mut body_hash_hex = String::with_capacity(body_hash.len() * 2);
    for b in body_hash {
        body_hash_hex.push_str(&format!("{:02x}", b));
    }

    format!("{}\n{}\n{}\n{}", prefix, timestamp, nonce, body_hash_hex)
}

fn current_unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
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

fn verify_submit_signature(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<(), String> {
    if state.config.dev_mode && state.config.allowed_orchestrator_pubkeys.is_empty() {
        return Ok(());
    }

    let pubkey_b64 = read_required_header(headers, "x-wqc-orchestrator-publickey")
        .map_err(|_| "Missing X-WQC-Orchestrator-PublicKey".to_string())?;
    if !state
        .config
        .allowed_orchestrator_pubkeys
        .contains(pubkey_b64)
    {
        return Err("Unauthorized Public Key".to_string());
    }

    let timestamp_str = read_required_header(headers, "x-wqc-timestamp")
        .map_err(|_| "Missing X-WQC-Timestamp".to_string())?;
    let timestamp = timestamp_str
        .parse::<i64>()
        .map_err(|_| "Invalid Timestamp".to_string())?;
    let now = current_unix_timestamp();
    if (now - timestamp).abs() > 300 {
        return Err("Timestamp Outside Allowed Window".to_string());
    }

    let nonce = read_required_header(headers, "x-wqc-nonce")
        .map_err(|_| "Missing X-WQC-Nonce".to_string())?;

    let signature_b64 = read_required_header(headers, "x-wqc-signature")
        .map_err(|_| "Missing X-WQC-Signature".to_string())?;
    let signature_bytes = STANDARD
        .decode(signature_b64)
        .map_err(|_| "Invalid Signature Encoding".to_string())?;
    let signature_arr: [u8; 64] = signature_bytes
        .try_into()
        .map_err(|_| "Invalid Signature Length".to_string())?;
    let signature = Signature::from_bytes(&signature_arr);

    let pubkey_bytes = STANDARD
        .decode(pubkey_b64)
        .map_err(|_| "Invalid Public Key Encoding".to_string())?;
    let pubkey_arr: [u8; 32] = pubkey_bytes
        .try_into()
        .map_err(|_| "Invalid Public Key Length".to_string())?;
    let verifying_key =
        VerifyingKey::from_bytes(&pubkey_arr).map_err(|_| "Invalid Public Key".to_string())?;

    let message = build_request_signature_message(body, timestamp, nonce);
    verifying_key
        .verify(message.as_bytes(), &signature)
        .map_err(|_| "Invalid Signature".to_string())?;

    let replay_key = format!("{}:{}", pubkey_b64, nonce);
    let cutoff = now - 300;
    let mut seen = state
        .seen_submit_nonces
        .lock()
        .map_err(|_| "Nonce state lock poisoned".to_string())?;
    seen.retain(|_, seen_at| *seen_at >= cutoff);
    if seen.contains_key(&replay_key) {
        return Err("Replay Detected (Duplicate Nonce)".to_string());
    }
    seen.insert(replay_key, now);

    Ok(())
}

fn read_required_header<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str, ()> {
    headers.get(name).and_then(|v| v.to_str().ok()).ok_or(())
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
