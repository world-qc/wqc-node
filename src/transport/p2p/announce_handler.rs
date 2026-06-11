use std::sync::Arc;

use futures::{AsyncReadExt, StreamExt};
use libp2p::PeerId;
use libp2p_stream::{Control, IncomingStreams};
use tokio::sync::Mutex;

use crate::application::state::AppState;
use crate::config::NodeConfig;
use crate::domain::p2p::TaskAnnouncement;
use crate::transport::p2p::bid_client::BidClient;

pub fn spawn_announce_handler(
    mut streams: IncomingStreams,
    bid_control: Arc<Mutex<Control>>,
    config: NodeConfig,
    state: Arc<AppState>,
    orchestrator_peer_id: PeerId,
) {
    tokio::spawn(async move {
        while let Some((peer_id, stream)) = streams.next().await {
            if peer_id != orchestrator_peer_id {
                tracing::warn!(
                    "[P2P Announce] Rejected announcement from unauthorized peer {}",
                    peer_id
                );
                continue;
            }

            let bid_control = bid_control.clone();
            let config = config.clone();
            let state = state.clone();
            tokio::spawn(async move {
                if let Err(e) =
                    handle_announce_stream(stream, bid_control, config, state, orchestrator_peer_id)
                        .await
                {
                    tracing::warn!("[P2P Announce] Failed to handle announce stream: {}", e);
                }
            });
        }
    });
}

async fn handle_announce_stream(
    mut stream: libp2p::Stream,
    bid_control: Arc<Mutex<Control>>,
    config: NodeConfig,
    state: Arc<AppState>,
    orchestrator_peer_id: PeerId,
) -> anyhow::Result<()> {
    let mut payload = Vec::new();
    stream.read_to_end(&mut payload).await?;

    let announcement: TaskAnnouncement = serde_json::from_slice(&payload)?;
    tracing::info!(
        "[P2P Announce] TaskAnnouncement task_id={} qubits={} difficulty={}",
        announcement.task_id,
        announcement.global_qubit_count,
        announcement.bid_difficulty
    );

    let client = BidClient::new(bid_control, config, state);
    client
        .submit_bid(announcement, orchestrator_peer_id)
        .await?;
    Ok(())
}
