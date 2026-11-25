use std::sync::Arc;

use futures::AsyncReadExt;

use crate::application::pcs_outbox;
use crate::application::state::AppState;
use crate::config::NodeConfig;
use crate::domain::pcs::{verify_pcs_request_signature, PcsRequestMessage};

/// Handles orchestrator PCS requests: this node was picked as slice proof
/// winner, so it is the only one that should build the leaf PCS bundle.
pub async fn handle_pcs_request_stream(
    state: Arc<AppState>,
    config: NodeConfig,
    mut stream: libp2p::Stream,
) -> anyhow::Result<()> {
    let mut payload = Vec::new();
    stream.read_to_end(&mut payload).await?;

    let message: PcsRequestMessage = serde_json::from_slice(&payload)?;

    let orchestrator_pubkey = config
        .orchestrator_public_key
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("orchestrator public key not configured"))?;

    verify_pcs_request_signature(&message.request, &message.signature, orchestrator_pubkey)
        .map_err(|e| anyhow::anyhow!("pcs request rejected: {e}"))?;

    if !message.request.node_id.is_empty() && message.request.node_id != config.peer_id {
        anyhow::bail!(
            "pcs request addressed to {}, not this node",
            message.request.node_id
        );
    }

    tracing::info!(
        "[P2P PCS Request] Accepted sub_task_id={} slice_id={} open_call={}",
        message.request.sub_task_id,
        message.request.slice_id,
        message.request.is_open_call()
    );

    pcs_outbox::handle_pcs_request(state, orchestrator_pubkey, &message.request);
    Ok(())
}
