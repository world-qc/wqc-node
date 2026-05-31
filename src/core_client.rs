use crate::models::{ComputeRequest, ComputeResponse, CoreSystemInfo};
use anyhow::{Context, Result};
use reqwest::{Client, StatusCode};
use std::time::Duration;

pub struct WqcCoreClient {
    client: Client,
    base_url: String,
}

impl WqcCoreClient {
    pub fn new(core_url: &str) -> Self {
        if core_url.starts_with("unix:") {
            // Extract the socket path from the "unix:" prefix (e.g., "unix:/var/run/wqc.sock")
            let socket_path = core_url.trim_start_matches("unix:");

            // Build a UDS-capable client only on Unix-like operating systems
            #[cfg(unix)]
            let client = Client::builder()
                .unix_socket(socket_path)
                .timeout(Duration::from_secs(10))
                .build()
                .expect("Failed to build UDS client");

            // Fallback client for non-Unix environments (e.g., Windows) to prevent compilation errors
            #[cfg(not(unix))]
            let client = Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("Failed to build HTTP client");

            Self {
                client,
                // For UDS, reqwest intercepts the transport layer and routes it through the socket file.
                // However, the high-level API still requires a valid HTTP base URL scheme, so we use a dummy domain.
                base_url: "http://localhost".to_string(),
            }
        } else {
            // Fallback to standard TCP routing (http:// or https://)
            Self {
                client: Client::builder()
                    .timeout(Duration::from_secs(10))
                    .build()
                    .expect("Failed to build HTTP client"),
                base_url: core_url.trim_end_matches('/').to_string(),
            }
        }
    }

    /// Dispatch a quantum computation task to wqc-core
    pub async fn dispatch_task(&self, request: ComputeRequest) -> Result<ComputeResponse> {
        let url = format!("{}/compute", self.base_url);

        let response = self.client
            .post(&url)
            // FIXME: Large-scale simulations might take time, setting a generous timeout
            .timeout(Duration::from_secs(300))
            .json(&request)
            .send()
            .await
            .context("Failed to send request to wqc-core")?;

        match response.status() {
            StatusCode::OK => {
                let res_body = response.json::<ComputeResponse>().await
                    .context("Failed to parse success response body")?;
                Ok(res_body)
            }
            status => {
                let error_text = response.text().await.unwrap_or_default();
                anyhow::bail!("wqc-core returned error status: {} - {}", status, error_text);
            }
        }
    }

    pub async fn get_supported_gates(&self) -> Result<Vec<String>> {
        let url = format!("{}/gates", self.base_url);

        let response = match self.client.get(&url).send().await {
            Ok(res) => res,
            Err(e) => {
                anyhow::bail!("Could not connect to wqc-core at {}: {}.", url, e);
            }
        };

        match response.json::<Vec<String>>().await {
            Ok(gates) => Ok(gates),
            Err(e) => {
                anyhow::bail!("Failed to parse gate list from core: {}.", e);
            }
        }
    }

    pub async fn get_system_info(&self) -> Result<CoreSystemInfo> {
        let url = format!("{}/sysinfo", self.base_url);

        let response = match self.client.get(&url).send().await {
            Ok(res) => res,
            Err(e) => {
                anyhow::bail!("Could not connect to wqc-core at {}: {}.", url, e);
            }
        };

        match response.json::<CoreSystemInfo>().await {
            Ok(data) => Ok(data),
            Err(e) => {
                anyhow::bail!("Failed to parse system info from core: {}.", e);
            }
        }
    }
}
