use crate::domain::models::{ComputeRequest, ComputeResponse, CoreSystemInfo, Proof};
use anyhow::{bail, Context, Result};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Consecutive unreachable errors before opening the circuit.
const DEFAULT_FAIL_THRESHOLD: u32 = 3;
/// Seconds to skip core calls after the circuit opens.
const DEFAULT_BACKOFF_SECS: u64 = 30;
/// Minimum interval between "core down; skipping" log lines.
const SKIP_LOG_INTERVAL: Duration = Duration::from_secs(30);

pub struct WqcCoreClient {
    client: Client,
    base_url: String,
    compute_timeout: Duration,
    pcs_timeout: Duration,
    fail_threshold: u32,
    backoff: Duration,
    consecutive_failures: AtomicU32,
    unhealthy_until: Mutex<Option<Instant>>,
    last_skip_log: Mutex<Option<Instant>>,
}

#[derive(Debug, Deserialize)]
pub struct LeafPcsResponse {
    pub leaf_pcs_b64: String,
    pub bytes: u64,
}

#[derive(Debug, Serialize)]
struct LeafPcsRequest {
    proof: Proof,
}

impl WqcCoreClient {
    pub fn new(core_url: &str, compute_timeout: Duration, pcs_timeout: Duration) -> Self {
        let fail_threshold = std::env::var("WQC_CORE_HEALTH_FAIL_THRESHOLD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_FAIL_THRESHOLD);
        let backoff_secs = std::env::var("WQC_CORE_HEALTH_BACKOFF_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_BACKOFF_SECS);

        if core_url.starts_with("unix:") {
            let socket_path = core_url.trim_start_matches("unix:");

            #[cfg(unix)]
            let client = Client::builder()
                .unix_socket(socket_path)
                .timeout(Duration::from_secs(10))
                .build()
                .expect("Failed to build UDS client");

            #[cfg(not(unix))]
            let client = Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("Failed to build HTTP client");

            Self {
                client,
                base_url: "http://localhost".to_string(),
                compute_timeout,
                pcs_timeout,
                fail_threshold,
                backoff: Duration::from_secs(backoff_secs),
                consecutive_failures: AtomicU32::new(0),
                unhealthy_until: Mutex::new(None),
                last_skip_log: Mutex::new(None),
            }
        } else {
            Self {
                client: Client::builder()
                    .timeout(Duration::from_secs(10))
                    .build()
                    .expect("Failed to build HTTP client"),
                base_url: core_url.trim_end_matches('/').to_string(),
                compute_timeout,
                pcs_timeout,
                fail_threshold,
                backoff: Duration::from_secs(backoff_secs),
                consecutive_failures: AtomicU32::new(0),
                unhealthy_until: Mutex::new(None),
                last_skip_log: Mutex::new(None),
            }
        }
    }

    /// True while the circuit is open (recent unreachable failures).
    pub fn is_in_backoff(&self) -> bool {
        self.unhealthy_until
            .lock()
            .expect("core health lock")
            .is_some_and(|until| Instant::now() < until)
    }

    /// Probe `GET /health` unless currently in backoff.
    /// Opens/closes the circuit based on reachability.
    pub async fn ensure_ready(&self) -> Result<()> {
        if self.is_in_backoff() {
            self.log_skip("wqc-core unhealthy (backoff); skipping request");
            bail!("wqc-core unhealthy (backoff); skipping request");
        }

        match self.probe_health().await {
            Ok(()) => {
                self.mark_healthy();
                Ok(())
            }
            Err(e) => {
                self.record_unreachable();
                Err(e).context("wqc-core health check failed")
            }
        }
    }

    pub async fn probe_health(&self) -> Result<()> {
        let url = format!("{}/health", self.base_url);
        let response = self
            .client
            .get(&url)
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .context("Failed to reach wqc-core /health")?;
        if !response.status().is_success() {
            bail!("wqc-core /health returned {}", response.status());
        }
        Ok(())
    }

    pub async fn dispatch_task(&self, request: ComputeRequest) -> Result<ComputeResponse> {
        self.ensure_ready().await?;

        let url = format!("{}/compute", self.base_url);
        let started = std::time::Instant::now();

        let response = match self
            .client
            .post(&url)
            .timeout(self.compute_timeout)
            .json(&request)
            .send()
            .await
        {
            Ok(response) => response,
            Err(e) => {
                let timed_out = e.is_timeout();
                if is_unreachable(&e) {
                    self.record_unreachable();
                }
                crate::infra::metrics::record_core_error(timed_out);
                return Err(e).context("Failed to send request to wqc-core");
            }
        };

        match response.status() {
            StatusCode::OK => {
                let res_body = response
                    .json::<ComputeResponse>()
                    .await
                    .context("Failed to parse success response body")?;
                self.mark_healthy();
                crate::infra::metrics::record_core_success(
                    started.elapsed(),
                    res_body.work_report.as_ref(),
                );
                Ok(res_body)
            }
            status => {
                crate::infra::metrics::record_core_error(false);
                let error_text = response.text().await.unwrap_or_default();
                anyhow::bail!(
                    "wqc-core returned error status: {} - {}",
                    status,
                    error_text
                )
            }
        }
    }

    /// Deferred leaf PCS construction (after result delivery).
    pub async fn build_leaf_pcs(&self, proof: Proof) -> Result<LeafPcsResponse> {
        self.post_leaf_pcs(LeafPcsRequest { proof }).await
    }

    /// Open-call builder path: CAS-fetched raw leaf STARK bytes → `/leaf_pcs`.
    pub async fn build_leaf_pcs_from_proof_bytes(
        &self,
        proof_bytes: &[u8],
        sub_task_id: &str,
        node_id: &str,
        slice_id: &str,
    ) -> Result<LeafPcsResponse> {
        use base64::{engine::general_purpose::STANDARD, Engine as _};

        let proof = Proof {
            public_inputs: crate::domain::models::PublicInputs {
                circuit_id: String::new(),
                sub_task_id: sub_task_id.to_string(),
                node_id: node_id.to_string(),
                slice_id: slice_id.to_string(),
                output_result_hash: String::new(),
                measurement_spec_hash: String::new(),
                security_level: String::new(),
            },
            stark_proof_b64: STANDARD.encode(proof_bytes),
        };
        self.build_leaf_pcs(proof).await
    }

    async fn post_leaf_pcs(&self, body: LeafPcsRequest) -> Result<LeafPcsResponse> {
        self.ensure_ready().await?;

        let url = format!("{}/leaf_pcs", self.base_url);
        let response = match self
            .client
            .post(&url)
            .timeout(self.pcs_timeout)
            .json(&body)
            .send()
            .await
        {
            Ok(response) => response,
            Err(e) => {
                if is_unreachable(&e) {
                    self.record_unreachable();
                }
                return Err(e).context("Failed to send leaf_pcs request to wqc-core");
            }
        };

        match response.status() {
            StatusCode::OK => {
                self.mark_healthy();
                response
                    .json::<LeafPcsResponse>()
                    .await
                    .context("Failed to parse leaf_pcs response")
            }
            status => {
                let error_text = response.text().await.unwrap_or_default();
                anyhow::bail!(
                    "wqc-core leaf_pcs error status: {} - {}",
                    status,
                    error_text
                )
            }
        }
    }

    pub async fn get_supported_gates(&self) -> Result<Vec<String>> {
        let url = format!("{}/gates", self.base_url);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("Could not connect to wqc-core at {}", url))?;

        response
            .json::<Vec<String>>()
            .await
            .with_context(|| "Failed to parse gate list from core")
    }

    pub async fn get_system_info(&self) -> Result<CoreSystemInfo> {
        let url = format!("{}/sysinfo", self.base_url);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("Could not connect to wqc-core at {}", url))?;

        response
            .json::<CoreSystemInfo>()
            .await
            .with_context(|| "Failed to parse system info from core")
    }

    fn mark_healthy(&self) {
        self.consecutive_failures.store(0, Ordering::SeqCst);
        *self.unhealthy_until.lock().expect("core health lock") = None;
    }

    fn record_unreachable(&self) {
        let n = self.consecutive_failures.fetch_add(1, Ordering::SeqCst) + 1;
        if n >= self.fail_threshold {
            let until = Instant::now() + self.backoff;
            *self.unhealthy_until.lock().expect("core health lock") = Some(until);
            self.consecutive_failures.store(0, Ordering::SeqCst);
            tracing::warn!(
                backoff_secs = self.backoff.as_secs(),
                threshold = self.fail_threshold,
                "wqc-core unreachable; opening health-gate backoff"
            );
        }
    }

    fn log_skip(&self, msg: &str) {
        let mut last = self.last_skip_log.lock().expect("core health lock");
        let now = Instant::now();
        if last.is_none_or(|t| now.duration_since(t) >= SKIP_LOG_INTERVAL) {
            tracing::warn!("{msg}");
            *last = Some(now);
        }
    }
}

fn is_unreachable(err: &reqwest::Error) -> bool {
    // Connection refused / UDS missing / DNS — not long prove/compute timeouts.
    if err.is_timeout() || err.is_body() || err.is_decode() {
        return false;
    }
    err.is_connect() || err.is_request()
}
