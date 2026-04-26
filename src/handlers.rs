use axum::{body::Bytes, extract::State, http::{HeaderMap, StatusCode}, Json};
use std::sync::{Arc, atomic::Ordering};
use crate::models::{ComputeRequest, NodeStatus};
use crate::AppState;
use crate::auth::verify_request_signature;
use crate::validation::validate_circuit_logic;
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, System};

pub async fn get_status(State(state): State<Arc<AppState>>) -> Json<NodeStatus> {
    let mut sys = System::new_all();
    // Refresh only what we need for performance
    sys.refresh_specifics(
        sysinfo::RefreshKind::new()
            .with_cpu(CpuRefreshKind::everything())
            .with_memory(MemoryRefreshKind::everything()),
    );

    Json(NodeStatus {
        node_public_key: state.config.node_public_key_b64.clone(),
        pending_tasks: state.pending_tasks.load(Ordering::SeqCst),
        max_qubits: state.config.max_qubits,
        max_memory_kb: state.config.max_memory_kb,
        system_memory_used_kb: sys.used_memory() / 1024,
        system_memory_total_kb: sys.total_memory() / 1024,
        cpu_usage_percent: sys.global_cpu_info().cpu_usage(),
        supported_gates: state.supported_gates.clone(),
    })
}

pub async fn submit_task(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, Json<serde_json::Value>) {
    if let Err(reason) = verify_request_signature(&state, &headers, &body) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": reason
            })),
        );
    }

    let payload: ComputeRequest = match serde_json::from_slice(&body) {
        Ok(payload) => payload,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!("Invalid JSON payload: {}", e)
                })),
            );
        }
    };

    tracing::info!("API: Validating task submission: {}", payload.task_id);

    // 1. Node Capacity Validation (Static Config)
    if payload.qubit_count > state.config.max_qubits {
        tracing::warn!("Rejecting: qubit_count exceeds node limit");
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "qubit_count exceeds node limit" })),
        );
    }

    if payload.memory_cost_kb > state.config.max_memory_kb {
        tracing::warn!("Rejecting: memory_cost_kb exceeds node limit");
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "memory_cost_kb exceeds node limit" })),
        );
    }

    // 2. Circuit Logic Validation (Dynamic Sync Data)
    // Pass the gates synced from wqc-core and the request's qubit_count
    if let Err(err_msg) = validate_circuit_logic(
        &payload.circuit,
        payload.qubit_count,
        &state.supported_gates
    ) {
        tracing::warn!("Rejecting: Circuit validation failed: {}", err_msg);
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": err_msg })),
        );
    }

    // 3. Queue the task
    state.pending_tasks.fetch_add(1, Ordering::SeqCst);
    if let Err(e) = state.task_sender.send(payload).await {
        tracing::error!("API: Failed to queue task: {}", e);
        state.pending_tasks.fetch_sub(1, Ordering::SeqCst);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "Failed to queue task" })),
        );
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "accepted",
            "message": "Task is valid and has been queued."
        })),
    )
}

pub async fn sync_core_capabilities(core_url: &str) -> Vec<String> {
    let client = reqwest::Client::new();
    let url = format!("{}/gates", core_url);

    // 1. Send request
    let response = match client.get(&url).send().await {
        Ok(res) => res,
        Err(e) => {
            tracing::warn!("Could not connect to wqc-core at {}: {}. Using default gates.", url, e);
            return default_gates();
        }
    };

    // 2. Parse JSON
    match response.json::<Vec<String>>().await {
        Ok(gates) => {
            tracing::info!("Synchronized with wqc-core. Supported gates: {:?}", gates);
            gates
        },
        Err(e) => {
            tracing::error!("Failed to parse gate list from core: {}. Using default gates.", e);
            default_gates()
        }
    }
}

// Default gate list as a fallback
fn default_gates() -> Vec<String> {
    vec!["H".into(), "X".into(), "Y".into(), "Z".into(), "CNOT".into()]
}
