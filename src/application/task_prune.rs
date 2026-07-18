use std::sync::Arc;
use std::time::Duration;

use crate::application::state::AppState;
use crate::infra::storage::Storage;

const DEFAULT_RETENTION_SECS: u64 = 86_400;
const DEFAULT_PRUNE_INTERVAL_SECS: u64 = 3_600;

/// Background loop that deletes old terminal `tasks` rows (see `Storage::prune_terminal_tasks`).
///
/// Env:
/// - `WQC_TASK_RETENTION_SECS` (default 86400). Set to `0` to disable pruning.
/// - `WQC_TASK_PRUNE_INTERVAL_SECS` (default 3600).
pub fn spawn_prune_loop(state: Arc<AppState>) {
    let retention_secs = std::env::var("WQC_TASK_RETENTION_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_RETENTION_SECS);

    if retention_secs == 0 {
        tracing::info!("[Task Prune] Disabled (WQC_TASK_RETENTION_SECS=0)");
        return;
    }

    let interval_secs = std::env::var("WQC_TASK_PRUNE_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_PRUNE_INTERVAL_SECS)
        .max(1);

    tokio::spawn(async move {
        let interval = Duration::from_secs(interval_secs);
        tracing::info!(
            "[Task Prune] Loop started (retention={}s, interval={}s)",
            retention_secs,
            interval_secs
        );

        loop {
            tokio::time::sleep(interval).await;
            run_prune(&state.storage, retention_secs);
        }
    });
}

fn run_prune(storage: &Storage, retention_secs: u64) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let cutoff = now.saturating_sub(retention_secs as i64);

    match storage.prune_terminal_tasks(cutoff) {
        Ok(0) => {}
        Ok(deleted) => {
            crate::infra::metrics::record_tasks_pruned(deleted);
            tracing::info!(deleted, cutoff, "[Task Prune] Removed terminal tasks");
        }
        Err(e) => {
            tracing::error!("[Task Prune] Failed: {}", e);
        }
    }
}
