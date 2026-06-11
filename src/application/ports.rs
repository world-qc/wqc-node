use async_trait::async_trait;

use crate::domain::models::{ComputeTask, TaskResultPayload};

#[async_trait]
pub trait TaskIngress: Send + Sync {
    async fn enqueue(&self, task: ComputeTask) -> Result<(), String>;
}

#[async_trait]
pub trait ResultSink: Send + Sync {
    /// Delivers a completed task result over the P2P result stream.
    async fn send_result(
        &self,
        task: &ComputeTask,
        payload: TaskResultPayload,
    ) -> anyhow::Result<()>;
}
