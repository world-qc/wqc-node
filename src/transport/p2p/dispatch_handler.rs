use std::sync::Arc;

use futures::{AsyncReadExt, StreamExt};
use libp2p::PeerId;
use libp2p_stream::IncomingStreams;

use crate::application::accept_task;
use crate::application::state::AppState;
use crate::config::NodeConfig;
use crate::domain::p2p::{verify_dispatch_signature, TaskDispatchMessage};

pub fn spawn_dispatch_handler(
    mut streams: IncomingStreams,
    state: Arc<AppState>,
    config: NodeConfig,
    orchestrator_peer_id: PeerId,
) {
    tokio::spawn(async move {
        while let Some((peer_id, stream)) = streams.next().await {
            if peer_id != orchestrator_peer_id {
                tracing::warn!(
                    "[P2P Dispatch] Rejected subtask from unauthorized peer {}",
                    peer_id
                );
                continue;
            }

            let state = state.clone();
            let config = config.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_dispatch_stream(state, config, stream).await {
                    tracing::warn!("[P2P Dispatch] Failed to handle dispatch stream: {}", e);
                }
            });
        }
    });
}

async fn handle_dispatch_stream(
    state: Arc<AppState>,
    config: NodeConfig,
    mut stream: libp2p::Stream,
) -> anyhow::Result<()> {
    let mut payload = Vec::new();
    stream.read_to_end(&mut payload).await?;

    let message: TaskDispatchMessage = serde_json::from_slice(&payload)?;
    let sub_task_id = message.sub_task.task_id.clone();
    let parent_task_id = message.sub_task.parent_task_id.clone();

    let orchestrator_pubkey = config
        .orchestrator_public_key
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("orchestrator public key not configured"))?;

    verify_dispatch_signature(
        &message.sub_task,
        &message.signature,
        orchestrator_pubkey,
    )
    .map_err(|e| anyhow::anyhow!("dispatch rejected: {e}"))?;

    let request = message
        .sub_task
        .into_compute_request(&config.peer_id);

    accept_task::accept_compute_task(&state, request, orchestrator_pubkey)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    tracing::info!(
        "[P2P Dispatch] Accepted sub_task_id={} parent_task_id={}",
        sub_task_id,
        parent_task_id
    );
    Ok(())
}
