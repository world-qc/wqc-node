#![deny(unused_imports)]

mod application;
mod config;
mod domain;
mod infra;
mod transport;

use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

use axum::{
    routing::get,
    Json, Router,
};
use colored::*;
use sysinfo::{MemoryRefreshKind, RefreshKind, System};
use tokio::sync::mpsc;

use std::time::Duration;

use application::state::AppState;
use application::worker;
use config::NodeConfig;
use infra::core_client::WqcCoreClient;
use transport::http::handlers;
use transport::p2p;
use transport::p2p::result_sink::P2pResultSink;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let config = NodeConfig::from_env()?;

    let core_client = Arc::new(WqcCoreClient::new(
        &config.core_url,
        Duration::from_secs(config.compute_timeout_secs),
    ));
    let supported_gates = handlers::sync_core_capabilities(core_client.clone()).await;

    let storage = infra::storage::Storage::new(&config.database_url)?;
    let pending_from_db = storage.get_pending_tasks()?;
    let pending_count = pending_from_db.len();
    let outbox_count = storage.count_pending_results().unwrap_or(0);

    print_startup_banner(&config, pending_count, outbox_count);

    let (tx, rx) = mpsc::channel(100);

    let shared_state = Arc::new(AppState {
        task_sender: tx.clone(),
        pending_tasks: AtomicUsize::new(pending_count),
        core_client,
        config: config.clone(),
        supported_gates,
        storage,
        p2p_stream_control: tokio::sync::Mutex::new(None),
    });

    for task in pending_from_db {
        tx.send(task).await?;
    }

    let result_sink: Arc<dyn application::ports::ResultSink> =
        Arc::new(P2pResultSink::new(shared_state.clone()));

    tokio::spawn(worker::start_worker(
        shared_state.clone(),
        result_sink,
        rx,
    ));

    p2p::host::spawn(config.clone(), shared_state.clone());

    application::result_outbox::spawn_retry_loop(shared_state.clone());

    let app = Router::new()
        .route("/status", get(handlers::get_status))
        .route(
            "/health",
            get(|| async { Json(serde_json::json!({ "status": "UP" })) }),
        )
        .with_state(shared_state);

    let bind_addr = format!("0.0.0.0:{}", config.http_port);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    tracing::info!("wqc-node HTTP admin API listening on {}", bind_addr);
    axum::serve(listener, app).await?;

    Ok(())
}

fn print_startup_banner(config: &NodeConfig, recovered_task_count: usize, outbox_count: usize) {
    let mut sys = System::new_all();
    sys.refresh_specifics(RefreshKind::new().with_memory(MemoryRefreshKind::everything()));
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

    println!(
        "  {} {}",
        "VISION:".dimmed(),
        "\"We are the Computer.\"".italic().bright_magenta()
    );
    println!("{}", "-".repeat(60).bright_blue());

    println!(
        "  {}  {:15} {}",
        "●".green(),
        "Status:".bold(),
        "Online & Ready".green()
    );
    println!(
        "  {}  {:15} {} GB / {} GB (Available/Total)",
        "●".blue(),
        "Memory:".bold(),
        avail_gb,
        total_gb
    );
    println!(
        "  {}  {:15} {} Max Qubits (Gate validation active)",
        "●".magenta(),
        "Capacity:".bold(),
        config.max_qubits
    );
    println!(
        "  {}  {:15} {} task(s) recovered from SQLite",
        "●".yellow(),
        "Storage:".bold(),
        recovered_task_count
    );
    if outbox_count > 0 {
        println!(
            "  {}  {:15} {} result(s) queued in outbox (will retry)",
            "●".yellow(),
            "Outbox:".bold(),
            outbox_count
        );
    }

    println!();
    println!(
        "  {} {}",
        "➜".bright_yellow(),
        "Node libp2p PeerID:".bold()
    );
    println!("    {}", config.peer_id.underline().bright_cyan());
    println!(
        "  {} {}",
        "➜".bright_yellow(),
        "P2P listen port:".bold()
    );
    println!(
        "    {}",
        config.p2p_listen_port.to_string().underline().bright_cyan()
    );
    println!(
        "  {} {}",
        "➜".bright_yellow(),
        "HTTP admin API:".bold()
    );
    println!(
        "    {}",
        format!("http://0.0.0.0:{}", config.http_port)
            .underline()
            .bright_cyan()
    );
    println!(
        "  {} {}",
        "➜".bright_yellow(),
        "Backend Core Connection:".bold()
    );
    println!("    {}", config.core_url.underline().bright_cyan());
    println!();
    println!("{}", "=".repeat(60).bright_blue());
    println!(
        "{}",
        "Node runtime initialized. Waiting for orchestrator workload over P2P...".dimmed()
    );
    println!();
}
