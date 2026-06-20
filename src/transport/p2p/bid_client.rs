use std::sync::atomic::Ordering;
use std::sync::Arc;

use futures::{AsyncWriteExt, StreamExt};
use libp2p::PeerId;
use libp2p_stream::{Control, IncomingStreams};
use tokio::sync::Mutex;

use crate::application::state::AppState;
use crate::config::NodeConfig;
use crate::domain::bid;
use crate::domain::features;
use crate::domain::p2p::TaskAnnouncement;
use crate::transport::p2p::stream_io::write_outbound_stream;

pub struct BidClient {
    control: Arc<Mutex<Control>>,
    config: NodeConfig,
    state: Arc<AppState>,
}

impl BidClient {
    pub fn new(control: Arc<Mutex<Control>>, config: NodeConfig, state: Arc<AppState>) -> Self {
        Self {
            control,
            config,
            state,
        }
    }

    pub async fn submit_bid(
        &self,
        announcement: TaskAnnouncement,
        orchestrator_peer_id: PeerId,
    ) -> anyhow::Result<()> {
        let supported_features = features::features_from_gates(&self.state.supported_gates);
        if !bid::should_bid_on(&announcement, &self.config, supported_features) {
            tracing::debug!(
                "[P2P Bid] Skipping task_id={} (capability/feature mismatch, supported_features=0x{:x})",
                announcement.task_id,
                supported_features
            );
            return Ok(());
        }

        let current_load = self.state.pending_tasks.load(Ordering::Relaxed) as u32;
        let signed_bid = bid::build_signed_bid(
            &announcement,
            &self.config,
            current_load,
            supported_features,
        )
            .ok_or_else(|| anyhow::anyhow!("failed to mine lottery proof within time window"))?;

        let payload = serde_json::to_vec(&signed_bid)?;
        write_outbound_stream(
            &self.control,
            orchestrator_peer_id,
            bid::PROTOCOL_BID,
            &payload,
        )
        .await?;

        tracing::info!(
            "[P2P Bid] Submitted bid for task_id={} to orchestrator {}",
            announcement.task_id,
            orchestrator_peer_id
        );
        Ok(())
    }

}

/// Drain unsolicited inbound streams on the bid protocol.
pub fn spawn_incoming_stream_sink(mut streams: IncomingStreams) {
    tokio::spawn(async move {
        while let Some((_peer, mut stream)) = streams.next().await {
            if let Err(e) = stream.close().await {
                tracing::debug!("[P2P Bid] Ignored inbound stream close error: {}", e);
            }
        }
    });
}
