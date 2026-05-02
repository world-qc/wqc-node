use std::sync::Arc;
use crate::AppState;
use crate::auth::generate_wqc_headers;
use reqwest::Client;
use serde_json::json;

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
