use axum::{extract::State, Json};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::application::state::AppState;
use crate::domain::models::NodeStatus;
use crate::infra::core_client::WqcCoreClient;

pub async fn sync_core_capabilities(core_client: Arc<WqcCoreClient>) -> Vec<String> {
    match core_client.get_supported_gates().await {
        Ok(gates) => {
            tracing::info!("Synchronized with wqc-core. Supported gates: {:?}", gates);
            gates
        }
        Err(e) => {
            tracing::error!(
                "Failed to sync core capabilities: {}. Using default gates.",
                e
            );
            default_gates()
        }
    }
}

fn default_gates() -> Vec<String> {
    vec![
        "H".into(),
        "X".into(),
        "Y".into(),
        "Z".into(),
        "CNOT".into(),
    ]
}

pub async fn collect_node_status(state: &AppState) -> NodeStatus {
    let outbox_pending = state.storage.count_pending_results().unwrap_or(0);
    match state.core_client.get_system_info().await {
        Ok(data) => NodeStatus {
            pending_tasks: state.pending_tasks.load(Ordering::SeqCst),
            outbox_pending,
            max_qubits: state.config.max_qubits,
            max_memory_gib: state.config.max_memory_gib,
            system_memory_used_kb: data.system_memory_used_kb,
            system_memory_total_kb: data.system_memory_total_kb,
            cpu_usage_percent: data.cpu_usage_percent,
            supported_gates: state.supported_gates.clone(),
        },
        Err(e) => {
            tracing::error!("Failed to get core system info: {}.", e);
            NodeStatus {
                pending_tasks: state.pending_tasks.load(Ordering::SeqCst),
                outbox_pending,
                max_qubits: state.config.max_qubits,
                max_memory_gib: state.config.max_memory_gib,
                system_memory_used_kb: 0,
                system_memory_total_kb: 0,
                cpu_usage_percent: 0.0,
                supported_gates: state.supported_gates.clone(),
            }
        }
    }
}

pub async fn get_status(State(state): State<Arc<AppState>>) -> Json<NodeStatus> {
    Json(collect_node_status(&state).await)
}
