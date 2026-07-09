use std::sync::Arc;

use async_trait::async_trait;

use crate::application::ports::ResultSink;
use crate::application::result_outbox;
use crate::application::state::AppState;
use crate::domain::models::{ComputeTask, TaskResultPayload};

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
            let error = payload.error.as_deref().unwrap_or("compute failed");
            tracing::warn!(
                "[P2P Result] Reporting compute failure for sub_task_id={}: {}",
                payload.task_id,
                error
            );
        }

        result_outbox::deliver_result(self.state.clone(), task, &payload).await
    }
}
