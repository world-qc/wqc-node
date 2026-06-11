use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

use async_trait::async_trait;
use libp2p_stream::Control;
use tokio::sync::mpsc;

use crate::application::ports::TaskIngress;
use crate::config::NodeConfig;
use crate::domain::models::ComputeTask;
use crate::infra::core_client::WqcCoreClient;
use crate::infra::storage::Storage;

pub struct AppState {
    pub task_sender: mpsc::Sender<ComputeTask>,
    pub pending_tasks: AtomicUsize,
    pub core_client: Arc<WqcCoreClient>,
    pub config: NodeConfig,
    pub supported_gates: Vec<String>,
    pub storage: Storage,
    pub p2p_stream_control: tokio::sync::Mutex<Option<Arc<tokio::sync::Mutex<Control>>>>,
}

#[async_trait]
impl TaskIngress for AppState {
    async fn enqueue(&self, task: ComputeTask) -> Result<(), String> {
        self.task_sender
            .send(task)
            .await
            .map_err(|_| "Worker queue is full".to_string())
    }
}
