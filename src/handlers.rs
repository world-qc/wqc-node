use axum::{body::Bytes, extract::State, http::{HeaderMap, StatusCode}, Json};
use std::sync::{Arc, atomic::Ordering};
use crate::models::{ComputeRequest, ComputeTask, NodeStatus};
use crate::AppState;
use crate::auth::verify_request_signature;
use crate::validation::validate_circuit_logic;
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, System};

pub async fn submit_task(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // 1. Identify the orchestrator's public key.
    // Verify the Ed25519 signature before proceeding.
    verify_request_signature(&state, &headers, &body)
        .map_err(|e| (StatusCode::UNAUTHORIZED, e))?;

    let pubkey = headers.get("X-WQC-Orchestrator-PublicKey")
        .and_then(|v| v.to_str().ok())
        .ok_or((StatusCode::BAD_REQUEST, "Missing X-WQC-Orchestrator-PublicKey header".to_string()))?
        .to_string();

    // 2. Parse and validate the compute request payload.
    let mut payload: ComputeRequest = serde_json::from_slice(&body)
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid JSON body".to_string()))?;

    // Node Capacity Validation (Static Config)
    if payload.qubit_count > state.config.max_qubits {
        return Err((StatusCode::BAD_REQUEST, "Requested qubit_count exceeds node limit".to_string()));
    }
    if payload.memory_cost_kb > state.config.max_memory_cost_kb {
        return Err((StatusCode::BAD_REQUEST, "Requested memory_cost_kb exceeds node limit".to_string()));
    }

    let difficulty = crate::validation::calculate_difficulty(&payload.circuit, payload.qubit_count);
    if difficulty < state.config.min_difficulty {
        return Err((StatusCode::BAD_REQUEST, "Difficulty too low: Task not economically viable".to_string()));
    }
    if difficulty > state.config.max_difficulty {
        return Err((StatusCode::BAD_REQUEST, "Difficulty too high: Exceeds node's computational capacity".to_string()));
    }
    tracing::info!("Accepted task {} with difficulty {}", payload.task_id, difficulty);
    payload.difficulty = Some(difficulty);

    // Circuit Logic Validation (Dynamic Sync Data)
    validate_circuit_logic(&payload.circuit, payload.qubit_count, &state.supported_gates)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    // Wrap into internal ComputeTask
    let task = ComputeTask {
        request: payload.clone(),
        orchestrator_pubkey: pubkey.clone(),
    };

    // 3. Persist the task to SQLite.
    // The composite unique key (pubkey + task_id) prevents duplicate submissions
    // from the same orchestrator while allowing ID overlap between different ones.
    state.storage.save_task(&task)
        .map_err(|e| {
            tracing::error!("Failed to persist task: {}", e);
            (StatusCode::CONFLICT, "Task already exists for this orchestrator or storage error".to_string())
        })?;

    let response_task_id = payload.task_id.clone();

    // 4. Dispatch the task to the background worker via MPSC channel.
    state.pending_tasks.fetch_add(1, Ordering::SeqCst);
    if let Err(e) = state.task_sender.send(task).await {
        tracing::error!("Task queue full: {}", e);
        state.pending_tasks.fetch_sub(1, Ordering::SeqCst);
        return Err((StatusCode::SERVICE_UNAVAILABLE, "Worker queue is full".to_string()));
    }

    Ok(Json(serde_json::json!({
        "status": "accepted",
        "task_id": response_task_id
    })))
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

pub fn collect_node_status(state: &AppState) -> NodeStatus {
    let mut sys = System::new_all();
    // Refresh only what we need for performance
    sys.refresh_specifics(
        sysinfo::RefreshKind::new()
            .with_cpu(CpuRefreshKind::everything())
            .with_memory(MemoryRefreshKind::everything()),
    );

    NodeStatus {
        pending_tasks: state.pending_tasks.load(Ordering::SeqCst),
        max_qubits: state.config.max_qubits,
        max_memory_cost_kb: state.config.max_memory_cost_kb,
        min_difficulty: state.config.min_difficulty,
        max_difficulty: state.config.max_difficulty,
        system_memory_used_kb: sys.used_memory() / 1024,
        system_memory_total_kb: sys.total_memory() / 1024,
        cpu_usage_percent: sys.global_cpu_info().cpu_usage(),
        supported_gates: state.supported_gates.clone(),
    }
}

pub async fn get_status(State(state): State<Arc<AppState>>) -> Json<NodeStatus> {
    Json(collect_node_status(&state))
}
