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

    // Retrieve the orchestrator's public key injected during the handler phase.
    // This is essential for identifying the correct row in the database.
    let pubkey = task.orchestrator_pubkey.clone().unwrap_or_else(|| "unknown".to_string());

    tracing::info!("Worker: Starting task {} for orchestrator {}", task_id, pubkey);

    // 1. Dispatch the quantum circuit execution to wqc-core.
    let result = core_client.dispatch_task(task).await;

    // Decrement the in-memory counter regardless of the execution result.
    state.pending_tasks.fetch_sub(1, Ordering::SeqCst);

    // 2. Prepare the result payload for the webhook.
    let payload = match result {
        Ok(res) => WebhookPayload {
            task_id: task_id.clone(),
            status: "success".to_string(),
            state_vector: Some(res.state_vector),
            proof: Some(res.proof),
            error: None,
        },
        Err(e) => {
            tracing::error!("Task {} failed: {}", task_id, e);
            WebhookPayload {
                task_id: task_id.clone(),
                status: "error".to_string(),
                state_vector: None,
                proof: None,
                error: Some(e.to_string()),
            }
        }
    };

    // 3. Update the task status in the database to 'completed'.
    // We use both the public key and task_id to ensure strict multi-tenant isolation.
    if let Err(e) = state.storage.update_status(&pubkey, &task_id, "completed") {
        tracing::error!("Storage update failed for task {} owned by {}: {}", task_id, pubkey, e);
        // We continue anyway to attempt webhook delivery.
    }

    // 4. Send the result back to the orchestrator via webhook if requested.
    if let Some(url) = webhook_url {
        // Note: send_webhook internal logic should handle signature signing
        // using our node's private key.
        if let Err(e) = crate::worker::send_webhook(state.clone(), http_client, &url, payload).await {
            tracing::error!("Webhook delivery failed for task {}: {}", task_id, e);
        }
    }

    tracing::info!("Worker: Finished task {}", task_id);
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
