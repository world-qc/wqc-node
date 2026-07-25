use std::sync::Arc;

use futures::{AsyncReadExt, StreamExt};
use libp2p::PeerId;
use libp2p_stream::IncomingStreams;

use crate::application::pcs_outbox;
use crate::application::state::AppState;
use crate::config::NodeConfig;
use crate::domain::pcs::{verify_pcs_request_signature, PcsRequestMessage};

/// Handles orchestrator PCS requests: this node was picked as slice proof
/// winner, so it is the only one that should build the leaf PCS bundle.
pub fn spawn_pcs_request_handler(
    mut streams: IncomingStreams,
    state: Arc<AppState>,
    config: NodeConfig,
    orchestrator_peer_id: PeerId,
) {
    tokio::spawn(async move {
        while let Some((peer_id, stream)) = streams.next().await {
            if peer_id != orchestrator_peer_id {
                tracing::warn!(
                    "[P2P PCS Request] Rejected request from unauthorized peer {}",
                    peer_id
                );
                continue;
            }

            let state = state.clone();
            let config = config.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_pcs_request_stream(state, config, stream).await {
                    tracing::warn!("[P2P PCS Request] Failed to handle request: {}", e);
                }
            });
        }
    });
}

async fn handle_pcs_request_stream(
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
        "[P2P PCS Request] Accepted sub_task_id={} slice_id={}",
        message.request.sub_task_id,
        message.request.slice_id
    );

    pcs_outbox::handle_pcs_request(state, orchestrator_pubkey, &message.request);
    Ok(())
}
