use std::sync::Arc;

use crate::application::state::AppState;
use crate::domain::models::{ComputeTask, TaskResultPayload};
use crate::domain::result::{ResultMessage, PROTOCOL_RESULT};
use crate::transport::p2p::stream_io::write_outbound_stream;

pub fn build_result_wire_body(
    task: &ComputeTask,
    payload: &TaskResultPayload,
    node_id: &str,
) -> anyhow::Result<Vec<u8>> {
    if payload.status != "success" {
        let error = payload
            .error
            .as_deref()
            .unwrap_or("compute failed");
        return ResultMessage::failure_json_bytes(&task.request.task_id, node_id, error);
    }

    let complex_result = payload
        .complex_result
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("missing complex_result for successful task"))?;
    let proof = payload
        .proof
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("missing proof for successful task"))?;

    let message = ResultMessage {
        sub_task_id: task.request.task_id.clone(),
        node_id: node_id.to_string(),
        complex_result: complex_result.clone(),
        proof: proof.clone(),
        work_report: payload.work_report.clone(),
        error: None,
    };

    message.to_json_bytes()
}

pub async fn send_result_wire(state: Arc<AppState>, wire_body: &[u8]) -> anyhow::Result<()> {
    let orchestrator_peer_id = state
        .config
        .orchestrator_peer_id
        .ok_or_else(|| anyhow::anyhow!("WQC_ORCHESTRATOR_BOOTSTRAP must include /p2p/<peer-id>"))?;

    let control = state
        .p2p_stream_control
        .lock()
        .await
        .clone()
        .ok_or_else(|| anyhow::anyhow!("P2P stream control is not ready yet"))?;

    write_outbound_stream(&control, orchestrator_peer_id, PROTOCOL_RESULT, wire_body).await?;

    tracing::info!(
        "[P2P Result] Delivered {} bytes to orchestrator {}",
        wire_body.len(),
        orchestrator_peer_id
    );
    Ok(())
}
