use std::sync::Arc;
use std::time::Duration;
use reqwest::Client;
use serde_json::json;
use crate::AppState;
use crate::auth::generate_wqc_headers;
use crate::handlers::collect_node_status;

pub async fn register_node(state: Arc<AppState>, orchestrator_url: &str) -> anyhow::Result<()> {
    let client = Client::new();
    let register_endpoint = format!("{}/api/v1/register", orchestrator_url);

    // The node's own address (This should be configurable or auto-detected)
    let body = json!({
        "address": state.config.node_url,
    });
    let body_bytes = serde_json::to_vec(&body)?;

    // Generate headers using the same logic as webhook results
    let (sig, pubkey, nonce, ts) = generate_wqc_headers(
        &state.config.signing_key,
        &body_bytes,
        "WQC-REGISTER-V1"
    );

    tracing::info!("Registering node to orchestrator: {}", register_endpoint);

    let response = client.post(&register_endpoint)
        .header("X-WQC-Signature", sig)
        .header("X-WQC-Node-PublicKey", pubkey)
        .header("X-WQC-Nonce", nonce)
        .header("X-WQC-Timestamp", ts)
        .json(&body)
        .send()
        .await?;

    if response.status().is_success() {
        if let Some(pubkey) = response.headers().get("X-WQC-Orchestrator-PublicKey") {
            if let Ok(pubkey_str) = pubkey.to_str() {
                let mut allowed = state.allowed_orchestrators.write().unwrap();
                allowed.insert(pubkey_str.to_string());
                tracing::info!("Successfully registered and trusted orchestrator: {}", pubkey_str);
            }
        }
        Ok(())
    } else {
        let err_msg = response.text().await?;
        Err(anyhow::anyhow!("Registration failed: {}", err_msg))
    }
}

pub async fn start_heartbeat_loop(state: Arc<AppState>, orchestrator_url: String) {
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_default();

    let heartbeat_endpoint = format!("{}/api/v1/heartbeat", orchestrator_url);
    let mut interval = tokio::time::interval(Duration::from_secs(60));

    tracing::info!("Starting heartbeat loop for {}", heartbeat_endpoint);

    loop {
        interval.tick().await;

        let status = collect_node_status(&state);
        let body_bytes = match serde_json::to_vec(&status) {
            Ok(b) => b,
            Err(e) => {
                tracing::error!("Failed to serialize node status: {}", e);
                continue;
            }
        };

        let (sig, pubkey, nonce, ts) = generate_wqc_headers(
            &state.config.signing_key,
            &body_bytes,
            "WQC-HEARTBEAT-V1"
        );

        match client.post(&heartbeat_endpoint)
            .header("X-WQC-Signature", sig)
            .header("X-WQC-Node-PublicKey", pubkey)
            .header("X-WQC-Nonce", nonce)
            .header("X-WQC-Timestamp", ts)
            .json(&status)
            .send()
            .await
        {
            Ok(res) if res.status().is_success() => {
                tracing::info!("Heartbeat sent to {}", orchestrator_url);
            }
            Ok(res) => {
                tracing::warn!("Heartbeat rejected by {}: Status {}", orchestrator_url, res.status());
            }
            Err(e) => {
                tracing::error!("Failed to send heartbeat to {}: {}", orchestrator_url, e);
            }
        }
    }
}
