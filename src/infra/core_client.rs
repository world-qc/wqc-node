use crate::domain::models::{ComputeRequest, ComputeResponse, CoreSystemInfo};
use anyhow::{Context, Result};
use reqwest::{Client, StatusCode};
use std::time::Duration;

pub struct WqcCoreClient {
    client: Client,
    base_url: String,
    compute_timeout: Duration,
}

impl WqcCoreClient {
    pub fn new(core_url: &str, compute_timeout: Duration) -> Self {
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
            }
        } else {
            Self {
                client: Client::builder()
                    .timeout(Duration::from_secs(10))
                    .build()
                    .expect("Failed to build HTTP client"),
                base_url: core_url.trim_end_matches('/').to_string(),
                compute_timeout,
            }
        }
    }

    pub async fn dispatch_task(&self, request: ComputeRequest) -> Result<ComputeResponse> {
        let url = format!("{}/compute", self.base_url);

        let response = self
            .client
            .post(&url)
            .timeout(self.compute_timeout)
            .json(&request)
            .send()
            .await
            .context("Failed to send request to wqc-core")?;

        match response.status() {
            StatusCode::OK => {
                let res_body = response
                    .json::<ComputeResponse>()
                    .await
                    .context("Failed to parse success response body")?;
                Ok(res_body)
            }
            status => {
                let error_text = response.text().await.unwrap_or_default();
                anyhow::bail!(
                    "wqc-core returned error status: {} - {}",
                    status,
                    error_text
                );
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
