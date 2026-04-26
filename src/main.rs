mod models;
mod core_client;
mod config;
mod handlers;
mod auth;
mod worker;
mod validation;

use models::ComputeRequest;

use axum::{routing::{get, post}, Router};
use std::sync::{atomic::AtomicUsize, Arc, Mutex};
use std::collections::HashMap;
use tokio::sync::mpsc;
use crate::config::NodeConfig;
use crate::core_client::WqcCoreClient;

// Global state shared across API handlers
struct AppState {
    task_sender: mpsc::Sender<ComputeRequest>,
    pending_tasks: AtomicUsize, // Counter for tasks currently in the queue or being processed
    config: NodeConfig,
    seen_submit_nonces: Mutex<HashMap<String, i64>>,
    supported_gates: Vec<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    // Load the configuration from environment variables here!
    let config = NodeConfig::from_env()?;

    // Fetch Core Capabilities
    let core_client = Arc::new(WqcCoreClient::new(&config.core_url));
    let supported_gates = handlers::sync_core_capabilities(&config.core_url).await;

    let (tx, rx) = mpsc::channel(100);
    let shared_state = Arc::new(AppState {
        task_sender: tx,
        pending_tasks: AtomicUsize::new(0),
        config: config.clone(),
        seen_submit_nonces: Mutex::new(HashMap::new()),
        supported_gates,
    });

    // Spawn background worker
    tokio::spawn(worker::start_worker(shared_state.clone(), core_client, rx));

    // Router
    let app = Router::new()
        .route("/status", get(handlers::get_status))
        .route("/submit", post(handlers::submit_task))
        .with_state(shared_state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    tracing::info!("wqc-node started on 0.0.0.0:8080");
    axum::serve(listener, app).await?;

    Ok(())
}
