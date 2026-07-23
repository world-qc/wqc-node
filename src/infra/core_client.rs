use crate::domain::models::{ComputeRequest, ComputeResponse, CoreSystemInfo, Proof};
use anyhow::{Context, Result};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub struct WqcCoreClient {
    client: Client,
    base_url: String,
    compute_timeout: Duration,
    pcs_timeout: Duration,
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
            }
        }
    }

    pub async fn dispatch_task(&self, request: ComputeRequest) -> Result<ComputeResponse> {
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
        let url = format!("{}/leaf_pcs", self.base_url);
        let response = self
            .client
            .post(&url)
            .timeout(self.pcs_timeout)
            .json(&LeafPcsRequest { proof })
            .send()
            .await
            .context("Failed to send leaf_pcs request to wqc-core")?;

        match response.status() {
            StatusCode::OK => response
                .json::<LeafPcsResponse>()
                .await
                .context("Failed to parse leaf_pcs response"),
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
}
