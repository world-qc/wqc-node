mod models;
mod core_client;
mod config;
mod handlers;
mod auth;
mod worker;
mod validation;
mod storage;
mod registration;

use models::ComputeTask;

use axum::{routing::{get, post}, Router, Json};
use std::sync::{atomic::AtomicUsize, Arc, Mutex, RwLock};
use std::collections::{HashMap, HashSet};
use tokio::sync::mpsc;
use crate::config::NodeConfig;
use crate::core_client::WqcCoreClient;

// Global state shared across API handlers
pub struct AppState {
    task_sender: mpsc::Sender<ComputeTask>,
    pending_tasks: AtomicUsize, // Counter for tasks currently in the queue or being processed
    config: NodeConfig,
    seen_submit_nonces: Mutex<HashMap<String, i64>>,
    supported_gates: Vec<String>,
    storage: storage::Storage,
    allowed_orchestrators: RwLock<HashSet<String>>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    // Load the configuration from environment variables here!
    let config = NodeConfig::from_env()?;

    // Fetch Core Capabilities
    let core_client = Arc::new(WqcCoreClient::new(&config.core_url));
    let supported_gates = handlers::sync_core_capabilities(&config.core_url).await;

    // Setup Storage
    let storage = storage::Storage::new(&config.database_url)?;
    let pending_from_db = storage.get_pending_tasks()?;
    tracing::info!("pending tasks: {:#?}", pending_from_db);
    let pending_count = pending_from_db.len();

    let allowed_orchestrators = RwLock::new(HashSet::<String>::new());

    // Setup Channel (Internal Queue)
    let (tx, rx) = mpsc::channel(100);

    // Build Shared State
    let shared_state = Arc::new(AppState {
        task_sender: tx.clone(), // Clone to send recovered tasks
        pending_tasks: AtomicUsize::new(pending_count),
        config: config.clone(),
        seen_submit_nonces: Mutex::new(HashMap::new()),
        supported_gates,
        storage,
        allowed_orchestrators,
    });

    // Recovery: Re-enqueue pending tasks from DB
    for task in pending_from_db {
        tx.send(task).await?;
    }

    // Spawn background worker (Engine communication)
    tokio::spawn(worker::start_worker(shared_state.clone(), core_client, rx));

    // Router
    let app = Router::new()
        .route("/status", get(handlers::get_status))
        .route("/submit", post(handlers::submit_task))
        .route("/health", get(|| async { Json(serde_json::json!({ "status": "UP" })) }))
        .with_state(shared_state.clone());

    // Trigger Orchestrator Registration (ASYNCHRONOUS)
    let state_for_reg = shared_state.clone();
    tokio::spawn(async move {
        if !state_for_reg.config.orchestrator_urls.is_empty() {
            tracing::info!("Auto-registration initiated for {} orchestrators...", state_for_reg.config.orchestrator_urls.len());

            for orch_url in &state_for_reg.config.orchestrator_urls {
                let state = state_for_reg.clone();
                let url = orch_url.clone();
                tokio::spawn(async move {
                    if let Err(e) = registration::register_node(state.clone(), &url).await {
                        tracing::error!("Registration failed for {}: {}", url, e);
                    } else {
                        tracing::info!("Successfully sent registration request to {}", url);
                        registration::start_heartbeat_loop(state, url).await;
                    }
                });
            }
        } else {
            tracing::warn!("No Orchestrator URLs configured. Node is running in standalone mode.");
        }
    });

    // Start API Server (This blocks the main thread)
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    tracing::info!("wqc-node started on 0.0.0.0:8080");
    axum::serve(listener, app).await?;

    Ok(())
}
