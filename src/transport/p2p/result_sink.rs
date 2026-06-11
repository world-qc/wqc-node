use std::sync::Arc;

use async_trait::async_trait;

use crate::application::ports::ResultSink;
use crate::application::state::AppState;
use crate::domain::models::{ComputeTask, TaskResultPayload};
use crate::domain::result::{ResultMessage, PROTOCOL_RESULT};
use crate::transport::p2p::stream_io::write_outbound_stream;

pub struct P2pResultSink {
    state: Arc<AppState>,
}

impl P2pResultSink {
    pub fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl ResultSink for P2pResultSink {
    async fn send_result(
        &self,
        task: &ComputeTask,
        payload: TaskResultPayload,
    ) -> anyhow::Result<()> {
        if payload.status != "success" {
            tracing::error!(
                "[P2P Result] Task {} failed locally; orchestrator requires a verified proof: {:?}",
                payload.task_id,
                payload.error
            );
            return Ok(());
        }

        let complex_result = payload
            .complex_result
            .ok_or_else(|| anyhow::anyhow!("missing complex_result for successful task"))?;
        let proof = payload
            .proof
            .ok_or_else(|| anyhow::anyhow!("missing proof for successful task"))?;

        let orchestrator_peer_id = self
            .state
            .config
            .orchestrator_peer_id
            .ok_or_else(|| anyhow::anyhow!("WQC_ORCHESTRATOR_BOOTSTRAP must include /p2p/<peer-id>"))?;

        let message = ResultMessage {
            sub_task_id: task.request.task_id.clone(),
            node_id: self.state.config.peer_id.clone(),
            complex_result,
            proof,
            error: None,
        };

        let body = message.to_json_bytes()?;
        let control = self
            .state
            .p2p_stream_control
            .lock()
            .await
            .clone()
            .ok_or_else(|| anyhow::anyhow!("P2P stream control is not ready yet"))?;

        write_outbound_stream(&control, orchestrator_peer_id, PROTOCOL_RESULT, &body).await?;

        tracing::info!(
            "[P2P Result] Submitted result for sub_task_id={} to orchestrator {}",
            task.request.task_id,
            orchestrator_peer_id
        );
        Ok(())
    }
}
