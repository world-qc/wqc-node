//! Prometheus metrics for `wqc-node` (`GET /metrics`).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use prometheus::{
    Encoder, Histogram, HistogramOpts, IntCounter, IntCounterVec, IntGauge, Opts, Registry,
    TextEncoder,
};
use serde::Serialize;

use crate::application::state::AppState;
use crate::domain::models::WorkReport;

static METRICS: OnceLock<NodeMetrics> = OnceLock::new();
static STARTED_AT: OnceLock<Instant> = OnceLock::new();

/// Shared flag updated by the P2P host when the orchestrator link is up/down.
static ORCHESTRATOR_CONNECTED: AtomicBool = AtomicBool::new(false);
static CONNECTED_PEERS: AtomicU64 = AtomicU64::new(0);
static CORE_TIMEOUTS: AtomicU64 = AtomicU64::new(0);

/// Compact health snapshot attached to bids (unsigned JSON extension).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MetricsSummary {
    pub pending_tasks: u64,
    pub outbox_pending: u64,
    pub p2p_connected_peers: u64,
    pub p2p_orchestrator_connected: u8,
    pub core_timeouts_total: u64,
    pub uptime_seconds: u64,
}

struct NodeMetrics {
    registry: Registry,
    uptime_seconds: IntGauge,
    pending_tasks: IntGauge,
    outbox_pending: IntGauge,
    p2p_connected_peers: IntGauge,
    p2p_orchestrator_connected: IntGauge,
    core_compute_duration_seconds: Histogram,
    core_prove_duration_seconds: Histogram,
    core_request_duration_seconds: Histogram,
    core_requests_total: IntCounterVec,
    core_timeouts_total: IntCounter,
    tasks_total: IntCounterVec,
    result_deliveries_total: IntCounterVec,
}

fn wall_ms_buckets() -> Vec<f64> {
    vec![
        0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0,
    ]
}

impl NodeMetrics {
    fn new() -> anyhow::Result<Self> {
        let registry = Registry::new();

        let info = IntGauge::with_opts(
            Opts::new("wqc_node_info", "Constant 1 labeled with node build metadata")
                .const_label("version", env!("CARGO_PKG_VERSION")),
        )?;
        info.set(1);

        let uptime_seconds =
            IntGauge::with_opts(Opts::new("wqc_node_uptime_seconds", "Process uptime in seconds"))?;
        let pending_tasks = IntGauge::with_opts(Opts::new(
            "wqc_node_pending_tasks",
            "Sub-tasks queued or in-flight on this node",
        ))?;
        let outbox_pending = IntGauge::with_opts(Opts::new(
            "wqc_node_outbox_pending",
            "Result outbox rows waiting for P2P delivery",
        ))?;
        let p2p_connected_peers = IntGauge::with_opts(Opts::new(
            "wqc_node_p2p_connected_peers",
            "Number of live libp2p peer connections",
        ))?;
        let p2p_orchestrator_connected = IntGauge::with_opts(Opts::new(
            "wqc_node_p2p_orchestrator_connected",
            "1 when connected to the orchestrator peer, else 0",
        ))?;

        let core_compute_duration_seconds = Histogram::with_opts(
            HistogramOpts::new(
                "wqc_node_core_compute_duration_seconds",
                "wqc-core compute wall time from WorkReport",
            )
            .buckets(wall_ms_buckets()),
        )?;
        let core_prove_duration_seconds = Histogram::with_opts(
            HistogramOpts::new(
                "wqc_node_core_prove_duration_seconds",
                "wqc-core prove wall time from WorkReport",
            )
            .buckets(wall_ms_buckets()),
        )?;
        let core_request_duration_seconds = Histogram::with_opts(
            HistogramOpts::new(
                "wqc_node_core_request_duration_seconds",
                "End-to-end POST /compute latency observed by the node",
            )
            .buckets(wall_ms_buckets()),
        )?;

        let core_requests_total = IntCounterVec::new(
            Opts::new(
                "wqc_node_core_requests_total",
                "POST /compute outcomes observed by the node",
            ),
            &["result"],
        )?;
        let core_timeouts_total = IntCounter::with_opts(Opts::new(
            "wqc_node_core_timeouts_total",
            "POST /compute requests that timed out",
        ))?;
        let tasks_total = IntCounterVec::new(
            Opts::new(
                "wqc_node_tasks_total",
                "Worker task outcomes (success or error payload)",
            ),
            &["status"],
        )?;
        let result_deliveries_total = IntCounterVec::new(
            Opts::new(
                "wqc_node_result_deliveries_total",
                "P2P result delivery attempts (initial and outbox retry)",
            ),
            &["attempt", "result"],
        )?;

        registry.register(Box::new(info))?;
        registry.register(Box::new(uptime_seconds.clone()))?;
        registry.register(Box::new(pending_tasks.clone()))?;
        registry.register(Box::new(outbox_pending.clone()))?;
        registry.register(Box::new(p2p_connected_peers.clone()))?;
        registry.register(Box::new(p2p_orchestrator_connected.clone()))?;
        registry.register(Box::new(core_compute_duration_seconds.clone()))?;
        registry.register(Box::new(core_prove_duration_seconds.clone()))?;
        registry.register(Box::new(core_request_duration_seconds.clone()))?;
        registry.register(Box::new(core_requests_total.clone()))?;
        registry.register(Box::new(core_timeouts_total.clone()))?;
        registry.register(Box::new(tasks_total.clone()))?;
        registry.register(Box::new(result_deliveries_total.clone()))?;

        Ok(Self {
            registry,
            uptime_seconds,
            pending_tasks,
            outbox_pending,
            p2p_connected_peers,
            p2p_orchestrator_connected,
            core_compute_duration_seconds,
            core_prove_duration_seconds,
            core_request_duration_seconds,
            core_requests_total,
            core_timeouts_total,
            tasks_total,
            result_deliveries_total,
        })
    }
}

/// Registers process metrics. Safe to call once at startup.
pub fn init() {
    STARTED_AT.get_or_init(Instant::now);
    METRICS.get_or_init(|| NodeMetrics::new().expect("failed to register node metrics"));
}

fn metrics() -> &'static NodeMetrics {
    METRICS
        .get()
        .expect("metrics::init() must be called at startup")
}

pub fn set_orchestrator_connected(connected: bool) {
    ORCHESTRATOR_CONNECTED.store(connected, Ordering::Relaxed);
    if let Some(m) = METRICS.get() {
        m.p2p_orchestrator_connected
            .set(if connected { 1 } else { 0 });
    }
}

pub fn set_connected_peers(count: usize) {
    CONNECTED_PEERS.store(count as u64, Ordering::Relaxed);
    if let Some(m) = METRICS.get() {
        m.p2p_connected_peers.set(count as i64);
    }
}

/// Builds the unsigned metrics summary carried on bids for orchestrator aggregation.
pub fn snapshot_for_bid(state: &AppState) -> MetricsSummary {
    MetricsSummary {
        pending_tasks: state.pending_tasks.load(Ordering::SeqCst) as u64,
        outbox_pending: state.storage.count_pending_results().unwrap_or(0) as u64,
        p2p_connected_peers: CONNECTED_PEERS.load(Ordering::Relaxed),
        p2p_orchestrator_connected: if ORCHESTRATOR_CONNECTED.load(Ordering::Relaxed) {
            1
        } else {
            0
        },
        core_timeouts_total: CORE_TIMEOUTS.load(Ordering::Relaxed),
        uptime_seconds: STARTED_AT
            .get()
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0),
    }
}

pub fn record_core_success(elapsed: Duration, work_report: Option<&WorkReport>) {
    let m = metrics();
    m.core_requests_total.with_label_values(&["ok"]).inc();
    m.core_request_duration_seconds
        .observe(elapsed.as_secs_f64());
    if let Some(report) = work_report {
        if report.compute_wall_ms > 0 {
            m.core_compute_duration_seconds
                .observe(report.compute_wall_ms as f64 / 1000.0);
        }
        if report.prove_wall_ms > 0 {
            m.core_prove_duration_seconds
                .observe(report.prove_wall_ms as f64 / 1000.0);
        }
    }
}

pub fn record_core_error(is_timeout: bool) {
    let m = metrics();
    if is_timeout {
        CORE_TIMEOUTS.fetch_add(1, Ordering::Relaxed);
        m.core_requests_total.with_label_values(&["timeout"]).inc();
        m.core_timeouts_total.inc();
    } else {
        m.core_requests_total.with_label_values(&["error"]).inc();
    }
}

pub fn record_task_status(status: &str) {
    metrics().tasks_total.with_label_values(&[status]).inc();
}

pub fn record_result_delivery(attempt: &str, result: &str) {
    metrics()
        .result_deliveries_total
        .with_label_values(&[attempt, result])
        .inc();
}

fn refresh_gauges(state: &AppState) {
    let m = metrics();
    m.uptime_seconds.set(
        STARTED_AT
            .get()
            .map(|t| t.elapsed().as_secs() as i64)
            .unwrap_or(0),
    );
    m.pending_tasks
        .set(state.pending_tasks.load(Ordering::SeqCst) as i64);
    let outbox = state.storage.count_pending_results().unwrap_or(0);
    m.outbox_pending.set(outbox as i64);
    m.p2p_orchestrator_connected
        .set(if ORCHESTRATOR_CONNECTED.load(Ordering::Relaxed) {
            1
        } else {
            0
        });
}

/// Periodically refreshes gauges that mirror SQLite / in-memory queue depth.
pub fn spawn_collector(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(15));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            refresh_gauges(&state);
        }
    });
}

fn encode_with_state(state: &AppState) -> Result<Vec<u8>, prometheus::Error> {
    refresh_gauges(state);
    let encoder = TextEncoder::new();
    let families = metrics().registry.gather();
    let mut buffer = Vec::new();
    encoder.encode(&families, &mut buffer)?;
    Ok(buffer)
}

pub async fn metrics_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match encode_with_state(&state) {
        Ok(body) => (
            StatusCode::OK,
            [(
                header::CONTENT_TYPE,
                "text/plain; version=0.0.4; charset=utf-8",
            )],
            body,
        )
            .into_response(),
        Err(e) => {
            tracing::error!("Failed to encode Prometheus metrics: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("metrics encode failed: {e}"),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_encode_includes_catalog() {
        init();
        record_core_error(true);
        record_result_delivery("retry", "error");
        set_orchestrator_connected(true);
        set_connected_peers(2);

        let encoder = TextEncoder::new();
        let families = metrics().registry.gather();
        let mut buffer = Vec::new();
        encoder.encode(&families, &mut buffer).expect("encode");
        let text = String::from_utf8(buffer).expect("utf8");
        assert!(text.contains("wqc_node_info"));
        assert!(text.contains("wqc_node_core_timeouts_total"));
        assert!(text.contains("wqc_node_p2p_orchestrator_connected"));
        assert!(text.contains("wqc_node_result_deliveries_total"));
    }
}
