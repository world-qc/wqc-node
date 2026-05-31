use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::sync::mpsc;
use crate::AppState;
use crate::auth::generate_wqc_headers;
use crate::models::{ComputeTask, ComputeRequest, WebhookPayload};
use crate::core_client::WqcCoreClient;

struct TaskResultData {
    content_hash: String,
    proof: crate::models::Proof,
    execution_time_ms: u64,
}

pub async fn start_worker(
    state: Arc<AppState>,
    mut rx: mpsc::Receiver<ComputeTask>,
) {
    let http_client = reqwest::Client::new();
    tracing::info!("Worker: Started task processing loop");

    while let Some(task) = rx.recv().await {
        process_task(state.clone(), &http_client, task).await;
    }
}

async fn process_task(
    state: Arc<AppState>,
    http_client: &reqwest::Client,
    mut task: ComputeTask,
) {
    let task_id = task.request.task_id.clone();
    let parent_task_id = task.request.parent_task_id.clone();
    let global_offset = task.request.global_offset.clone();
    let webhook_url = task.request.webhook_url.take();

    // Retrieve the orchestrator's public key injected during the handler phase.
    // This is essential for identifying the correct row in the database.
    let pubkey = task.orchestrator_pubkey.clone();

    tracing::info!("Worker: Starting task {}", task_id);

    // Call the abstracted logic
    let result = execute_compute_and_upload(&state.core_client, http_client, &task.request).await;

    // 2. Prepare the result payload for the webhook.
    let payload = match result {
        Ok(data) => WebhookPayload {
            task_id: task_id.clone(),
            parent_task_id,
            global_offset,
            status: "success".to_string(),
            content_hash: Some(data.content_hash),
            proof: Some(data.proof),
            error: None,
            execution_time_ms: Some(data.execution_time_ms),
        },
        Err(e) => {
            tracing::error!("Task {} failed: {}", task_id, e);
            WebhookPayload {
                task_id: task_id.clone(),
                parent_task_id,
                global_offset,
                status: "error".to_string(),
                content_hash: None,
                proof: None,
                error: Some(e.to_string()),
                execution_time_ms: None,
            }
        }
    };

    // 3. Update the task status in the database to 'completed' or 'failed'.
    // We use both the public key and task_id to ensure strict multi-tenant isolation.
    let status = if payload.status == "error" { "failed" } else { "completed" };
    if let Err(e) = state.storage.update_status(&pubkey, &task_id, status) {
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

    // Decrement the in-memory counter regardless of the execution result.
    state.pending_tasks.fetch_sub(1, Ordering::SeqCst);

    tracing::info!("Worker: Finished task {}", task_id);
}

// Core logic that handles computation and S3 upload
// Returns Success data or an Error
async fn execute_compute_and_upload(
    core_client: &WqcCoreClient,
    http_client: &reqwest::Client,
    request: &ComputeRequest,
) -> anyhow::Result<TaskResultData> {
    // Start timer
    let start_time = std::time::Instant::now();

    // 1. Dispatch computation (?) handles early return on error
    let res = core_client.dispatch_task(request.clone()).await?;

    // Calculate wall-clock time
    let execution_time_ms = start_time.elapsed().as_millis() as u64;

    // 2. Binary conversion
    let mut binary_data = Vec::with_capacity(res.state_vector.len() * 16);
    for [real, imag] in &res.state_vector {
        let r_bytes = real.to_le_bytes();
        let i_bytes = imag.to_le_bytes();
        binary_data.extend_from_slice(&r_bytes);
        binary_data.extend_from_slice(&i_bytes);
    }

    // 3. Upload to S3 if URL is provided
    if let Some(url) = &request.upload_url {
        let resp = http_client.put(url)
            .header("Content-Type", "application/octet-stream")
            .body(binary_data)
            .send()
            .await?; // HTTP request error

        if !resp.status().is_success() {
            anyhow::bail!("S3 upload failed with status: {}", resp.status());
        }
    }

    Ok(TaskResultData {
        content_hash: res.proof.public_inputs.output_result_hash.clone(),
        proof: res.proof,
        execution_time_ms,
    })
}

async fn send_webhook(
    state: Arc<AppState>,
    client: &reqwest::Client,
    url: &str,
    payload: WebhookPayload,
) -> anyhow::Result<()> {
    let body_json = serde_json::to_string(&payload)?;
    let body_bytes = body_json.as_bytes();

    // Generate headers using the same logic as webhook results
    let (sig, pubkey, nonce, ts) = generate_wqc_headers(
        &state.config.signing_key,
        body_bytes,
        "WQC-WEBHOOK-V1"
    );

    let res = client.post(url)
        .header("X-WQC-Node-PublicKey", pubkey)
        .header("X-WQC-Timestamp", ts)
        .header("X-WQC-Nonce", nonce)
        .header("X-WQC-Signature", sig)
        .header("Content-Type", "application/json")
        .body(body_json)
        .send()
        .await?;

    if !res.status().is_success() {
        let status = res.status();
        let detail = res.text().await.unwrap_or_default();
        tracing::warn!("Worker: Webhook target returned error {}: {}", status, detail);
    } else {
        tracing::info!("Webhook: Notified {} for task {}", url, payload.task_id);
    }

    Ok(())
}
