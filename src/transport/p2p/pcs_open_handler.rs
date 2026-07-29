use std::sync::Arc;

use futures::{AsyncReadExt, StreamExt};
use libp2p::PeerId;
use libp2p_stream::{Control, IncomingStreams};
use tokio::sync::Mutex;

use crate::application::pcs_open_call::{
    cache_open_call, fetch_core_pcs_memory_policy, should_bid_open_call, skip_bid_reason,
};
use crate::application::state::AppState;
use crate::config::NodeConfig;
use crate::domain::pcs::{verify_pcs_open_call_signature, PcsOpenCallMessage};
use crate::transport::p2p::pcs_bid_client::submit_pcs_open_call_bid;

/// Handles orchestrator CAS PCS open-call announcements.
/// Spill-policy cores bid; refuse-policy cores stay silent.
pub fn spawn_pcs_open_handler(
    mut streams: IncomingStreams,
    control: Arc<Mutex<Control>>,
    state: Arc<AppState>,
    config: NodeConfig,
    orchestrator_peer_id: PeerId,
) {
    tokio::spawn(async move {
        while let Some((peer_id, stream)) = streams.next().await {
            if peer_id != orchestrator_peer_id {
                tracing::warn!(
                    "[P2P PCS Open] Rejected open call from unauthorized peer {}",
                    peer_id
                );
                continue;
            }

            let control = control.clone();
            let state = state.clone();
            let config = config.clone();
            tokio::spawn(async move {
                if let Err(e) =
                    handle_pcs_open_stream(control, state, config, orchestrator_peer_id, stream)
                        .await
                {
                    tracing::warn!("[P2P PCS Open] Failed to handle open call: {}", e);
                }
            });
        }
    });
}

async fn handle_pcs_open_stream(
    control: Arc<Mutex<Control>>,
    state: Arc<AppState>,
    config: NodeConfig,
    orchestrator_peer_id: PeerId,
    mut stream: libp2p::Stream,
) -> anyhow::Result<()> {
    let mut payload = Vec::new();
    stream.read_to_end(&mut payload).await?;

    let message: PcsOpenCallMessage = serde_json::from_slice(&payload)?;

    let orchestrator_pubkey = config
        .orchestrator_public_key
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("orchestrator public key not configured"))?;

    verify_pcs_open_call_signature(&message.open_call, &message.signature, orchestrator_pubkey)
        .map_err(|e| anyhow::anyhow!("pcs open call rejected: {e}"))?;

    let open = &message.open_call;
    let core_policy = fetch_core_pcs_memory_policy(&state).await;

    // Always cache for spill cores so a later pcs-req can fetch the CAS blob.
    if core_policy.is_spill() {
        cache_open_call(&state, open.clone()).await;
    }

    if !should_bid_open_call(&config, core_policy, open) {
        tracing::debug!(
            "[P2P PCS Open] Skipping bid: {} sub_task_id={} core_policy={}",
            skip_bid_reason(&config, core_policy, open),
            open.sub_task_id,
            core_policy.as_str()
        );
        return Ok(());
    }

    tracing::info!(
        "[P2P PCS Open] Accepted open call sub_task_id={} slice_id={} leaf_proof_hash={} r_pcs={}",
        open.sub_task_id,
        open.slice_id,
        open.leaf_proof_hash,
        open.r_pcs_planck
    );

    submit_pcs_open_call_bid(&control, &config, open, orchestrator_peer_id).await
}
