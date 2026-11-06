use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::{AsyncWriteExt, StreamExt};
use libp2p::PeerId;
use libp2p_stream::{Control, IncomingStreams};
use tokio::sync::Mutex;

use crate::application::state::AppState;
use crate::config::NodeConfig;
use crate::domain::bid;
use crate::domain::features;
use crate::domain::p2p::TaskAnnouncement;
use crate::domain::work::BudgetPolicy;
use crate::infra::geoip;
use crate::transport::p2p::stream_io::write_outbound_stream_expect_ack;

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
        let supported_features =
            features::node_features(&self.state.supported_gates, self.config.tn_rates.is_some());
        if !bid::should_bid_on(&announcement, &self.config, supported_features) {
            tracing::debug!(
                "[P2P Bid] Skipping task_id={} (capability/feature mismatch, supported_features=0x{:x})",
                announcement.task_id,
                supported_features
            );
            return Ok(());
        }
        // Bit 4 is set only with rates, so announced work always has them here.
        if let (Some(work), Some(rates)) = (announcement.work, self.config.tn_rates) {
            let policy = BudgetPolicy {
                rates,
                overhead_secs: self.config.work_overhead_secs,
                margin: self.config.work_budget_margin,
                own_timeout_secs: self.config.compute_timeout_secs,
                unknown_secs: self.config.work_unknown_secs,
            };
            let deadline_secs = announcement.deadline_secs.unwrap_or(0);
            let decision = self.state.work.lock().expect("work ledger").decide(
                &announcement.task_id,
                &work,
                deadline_secs,
                &policy,
                Instant::now(),
            );
            if let Err(expected) = decision {
                tracing::info!(
                    "[P2P Bid] Skipping task_id={} (expected {:.0}s with queue, past {:.2} of min(deadline {}s, timeout {}s))",
                    announcement.task_id,
                    expected,
                    policy.margin,
                    deadline_secs,
                    policy.own_timeout_secs
                );
                return Ok(());
            }
        }

        let current_load = self.state.pending_tasks.load(Ordering::Relaxed) as u32;
        let location =
            geoip::resolve_node_location(&self.state.storage, &self.state.http_client).await;
        let metrics_summary = Some(crate::infra::metrics::snapshot_for_bid(&self.state));
        let signed_bid = bid::build_signed_bid(
            &announcement,
            &self.config,
            current_load,
            supported_features,
            location,
            metrics_summary,
        )
        .ok_or_else(|| anyhow::anyhow!("failed to mine lottery proof within time window"))?;

        let payload = serde_json::to_vec(&signed_bid)?;
        const MAX_ATTEMPTS: u32 = 3;
        let mut last_err = None;
        for attempt in 0..MAX_ATTEMPTS {
            // The proxy reads one bid stream at a time and does not confirm a
            // bare write. Wait for its ack, and retry while the bid window is open.
            match write_outbound_stream_expect_ack(
                &self.control,
                orchestrator_peer_id,
                bid::PROTOCOL_BID,
                &payload,
            )
            .await
            {
                Ok(()) => {
                    last_err = None;
                    break;
                }
                Err(e) => {
                    tracing::warn!(
                        "[P2P Bid] Submit attempt {} failed for task_id={}: {}",
                        attempt + 1,
                        announcement.task_id,
                        e
                    );
                    last_err = Some(e);
                    if attempt + 1 < MAX_ATTEMPTS {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            }
        }
        if let Some(e) = last_err {
            return Err(e);
        }

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
