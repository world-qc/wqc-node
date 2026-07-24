use std::sync::Arc;

use crate::application::state::AppState;
use crate::domain::pcs::{PcsMessage, PROTOCOL_PCS};
use crate::transport::p2p::stream_io::write_outbound_stream_expect_ack;

pub async fn send_pcs_wire(state: Arc<AppState>, wire_body: &[u8]) -> anyhow::Result<()> {
    let orchestrator_peer_id = state
        .config
        .orchestrator_peer_id
        .ok_or_else(|| anyhow::anyhow!("orchestrator peer id not configured"))?;

    let control = state
        .p2p_stream_control
        .lock()
        .await
        .clone()
        .ok_or_else(|| anyhow::anyhow!("P2P stream control is not ready yet"))?;

    write_outbound_stream_expect_ack(&control, orchestrator_peer_id, PROTOCOL_PCS, wire_body)
        .await?;

    tracing::info!(
        "[P2P PCS] Delivered {} bytes to orchestrator {} (acked)",
        wire_body.len(),
        orchestrator_peer_id
    );
    Ok(())
}

pub fn build_pcs_wire_body(
    sub_task_id: &str,
    node_id: &str,
    leaf_pcs_b64: &str,
) -> anyhow::Result<Vec<u8>> {
    PcsMessage {
        sub_task_id: sub_task_id.to_string(),
        node_id: node_id.to_string(),
        leaf_pcs_b64: leaf_pcs_b64.to_string(),
    }
    .to_json_bytes()
}
