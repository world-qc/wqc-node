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

use colored::*;
use sysinfo::{System, RefreshKind, MemoryRefreshKind};

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
    let supported_gates = handlers::sync_core_capabilities(core_client.clone()).await;

    // Setup Storage
    let storage = storage::Storage::new(&config.database_url)?;
    let pending_from_db = storage.get_pending_tasks()?;
    let pending_count = pending_from_db.len();

    print_startup_banner(&config, pending_count);

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
        allowed_orchestrators: RwLock::new(HashSet::<String>::new()),
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

fn print_startup_banner(config: &NodeConfig, recovered_task_count: usize) {
    let mut sys = System::new_all();
    sys.refresh_specifics(
        RefreshKind::new().with_memory(MemoryRefreshKind::everything()),
    );
    let total_gb = sys.total_memory() / 1024 / 1024 / 1024;
    let avail_gb = sys.available_memory() / 1024 / 1024 / 1024;

    println!("{}", "=".repeat(60).bright_blue());

    let wqc_logo = r#"
██╗    ██╗  ██████╗   ██████╗
██║    ██║ ██╔═══██╗ ██╔════╝
██║ █╗ ██║ ██║   ██║ ██║
██║███╗██║ ██║▄▄ ██║ ██║
╚███╔███╔╝ ╚██████╔╝ ╚██████╗
 ╚══╝╚══╝   ╚══▀▀═╝   ╚═════╝ worker-node
    "#;
    println!("{}", wqc_logo.bright_cyan().bold());

    println!("  {} {}", "VISION:".dimmed(), "\"We are the Computer.\"".italic().bright_magenta());
    println!("{}", "-".repeat(60).bright_blue());

    // Status display
    println!(
        "  {}  {:15} {}",
        "●".green(), "Status:".bold(), "Online & Ready".green()
    );
    println!(
        "  {}  {:15} {} GB / {} GB (Available/Total)",
        "●".blue(), "Memory:".bold(), avail_gb, total_gb
    );
    println!(
        "  {}  {:15} {} Max Qubits (Gate validation active)",
        "●".magenta(), "Capacity:".bold(), config.max_qubits
    );
    println!(
        "  {}  {:15} {} task(s) recovered from SQLite",
        "●".yellow(), "Storage:".bold(), recovered_task_count
    );

    // Network information
    println!();
    println!("  {} {}", "➜".bright_yellow(), "Node API Endpoint (Inbound):".bold());
    println!("    {}", "http://0.0.0.0:8080".underline().bright_cyan());
    println!("  {} {}", "➜".bright_yellow(), "Backend Core Connection (Outbound):".bold());
    println!("    {}", config.core_url.underline().bright_cyan());
    println!();
    println!("{}", "=".repeat(60).bright_blue());
    println!("{}", "Node runtime initialized. Waiting for orchestrator workload...".dimmed());
    println!();
}
