use std::sync::Arc;

use futures::AsyncReadExt;

use crate::application::accept_task;
use crate::application::state::AppState;
use crate::config::NodeConfig;
use crate::domain::p2p::{verify_dispatch_signature, TaskDispatchMessage};

pub async fn handle_dispatch_stream(
    state: Arc<AppState>,
    config: NodeConfig,
    mut stream: libp2p::Stream,
) -> anyhow::Result<()> {
    let mut payload = Vec::new();
    stream.read_to_end(&mut payload).await?;

    let message: TaskDispatchMessage = serde_json::from_slice(&payload)?;
    let sub_task_id = message.sub_task.task_id.clone();
    let parent_task_id = message.sub_task.parent_task_id.clone();

    let orchestrator_pubkey = config
        .orchestrator_public_key
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("orchestrator public key not configured"))?;

    verify_dispatch_signature(&message.sub_task, &message.signature, orchestrator_pubkey)
        .map_err(|e| anyhow::anyhow!("dispatch rejected: {e}"))?;

    let request = message.sub_task.into_compute_request(&config.peer_id);
    let budget = state
        .work
        .lock()
        .expect("work ledger")
        .budget(&parent_task_id);

    accept_task::accept_compute_task(&state, request, orchestrator_pubkey, budget)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    tracing::info!(
        "[P2P Dispatch] Accepted sub_task_id={} parent_task_id={}",
        sub_task_id,
        parent_task_id
    );
    Ok(())
}
