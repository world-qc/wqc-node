use std::sync::Arc;
use std::time::Duration;

use libp2p::PeerId;
use tokio::sync::Mutex;

use crate::config::NodeConfig;
use crate::domain::pcs::{build_signed_pcs_bid, PcsOpenCall, PROTOCOL_PCS_BID};
use crate::transport::p2p::stream_io::write_outbound_stream;
use libp2p_stream::Control;

/// Submits a signed spill-policy PCS open-call bid to the orchestrator.
pub async fn submit_pcs_open_call_bid(
    control: &Arc<Mutex<Control>>,
    config: &NodeConfig,
    open: &PcsOpenCall,
    orchestrator_peer_id: PeerId,
) -> anyhow::Result<()> {
    let message = build_signed_pcs_bid(open, &config.peer_id, &config.signing_key);
    if !message.bid.is_spill_policy() {
        anyhow::bail!("pcs open-call bid must declare pcs_memory_policy=spill");
    }
    let payload = serde_json::to_vec(&message)?;

    const MAX_ATTEMPTS: u32 = 3;
    let mut last_err = None;
    for attempt in 0..MAX_ATTEMPTS {
        match write_outbound_stream(control, orchestrator_peer_id, PROTOCOL_PCS_BID, &payload).await
        {
            Ok(()) => {
                tracing::info!(
                    "[P2P PCS Bid] Submitted open-call bid sub_task_id={} leaf_proof_hash={}",
                    open.sub_task_id,
                    open.leaf_proof_hash
                );
                return Ok(());
            }
            Err(e) => {
                tracing::warn!(
                    "[P2P PCS Bid] Submit attempt {} failed for sub_task_id={}: {}",
                    attempt + 1,
                    open.sub_task_id,
                    e
                );
                last_err = Some(e);
                if attempt + 1 < MAX_ATTEMPTS {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("pcs bid submit failed")))
}
