use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use crate::AppState;
use crate::models::{ComputeRequest, WebhookPayload};
use crate::core_client::WqcCoreClient;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use ed25519_dalek::Signer;
use sha2::Digest;

pub async fn start_worker(
    state: Arc<AppState>,
    core_client: Arc<WqcCoreClient>,
    mut rx: mpsc::Receiver<ComputeRequest>,
) {
    let http_client = reqwest::Client::new();
    tracing::info!("Worker: Started task processing loop");

    while let Some(task) = rx.recv().await {
        process_task(state.clone(), core_client.clone(), &http_client, task).await;
    }
}

async fn process_task(
    state: Arc<AppState>,
    core_client: Arc<WqcCoreClient>,
    http_client: &reqwest::Client,
    task: ComputeRequest,
) {
    let task_id = task.task_id.clone();
    let webhook_url = task.webhook_url.clone();

    tracing::info!("Worker: Processing task {}", task_id);

    // Call wqc-core
    let result = core_client.dispatch_task(task).await;

    // Decrement pending tasks count
    state.pending_tasks.fetch_sub(1, Ordering::SeqCst);

    let payload = match result {
        Ok(res) => WebhookPayload {
            task_id,
            status: "success".to_string(),
            state_vector: Some(res.state_vector),
            proof: Some(res.proof),
            error: None,
        },
        Err(e) => WebhookPayload {
            task_id,
            status: "error".to_string(),
            state_vector: None,
            proof: None,
            error: Some(e.to_string()),
        },
    };

    if let Some(url) = webhook_url {
        if let Err(e) = send_webhook(state, http_client, &url, payload).await {
            tracing::error!("Worker: Webhook delivery failed: {}", e);
        }
    }
}

async fn send_webhook(
    state: Arc<AppState>,
    client: &reqwest::Client,
    url: &str,
    payload: WebhookPayload,
) -> anyhow::Result<()> {
    let body_json = serde_json::to_string(&payload)?;

    // Signing logic for Webhook (Ed25519)
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let nonce = uuid::Uuid::new_v4().to_string();

    // Create structured message for signing
    let mut hasher = sha2::Sha256::new();
    hasher.update(body_json.as_bytes());
    let body_hash = hex::encode(hasher.finalize());

    let message = format!("WQC-WEBHOOK-V1\n{}\n{}\n{}", now, nonce, body_hash);
    let signature = state.config.signing_key.sign(message.as_bytes());
    let signature_b64 = STANDARD.encode(signature.to_bytes());

    let res = client.post(url)
        .header("X-WQC-Node-PublicKey", &state.config.node_public_key_b64)
        .header("X-WQC-Timestamp", now.to_string())
        .header("X-WQC-Nonce", nonce)
        .header("X-WQC-Signature", signature_b64)
        .json(&payload)
        .send()
        .await?;

    if !res.status().is_success() {
        tracing::warn!("Worker: Webhook target returned error: {}", res.status());
    } else {
        tracing::info!("Webhook: Notified {} for task {}", url, payload.task_id);
    }

    Ok(())
}
