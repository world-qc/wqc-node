use crate::models::{ComputeRequest, ComputeResponse};
use anyhow::{Context, Result};
use reqwest::{Client, StatusCode};
use std::time::Duration;

pub struct WqcCoreClient {
    client: Client,
    base_url: String,
}

impl WqcCoreClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            client: Client::builder()
                // Large-scale simulations might take time, setting a generous timeout
                .timeout(Duration::from_secs(300))
                .build()
                .expect("Failed to build HTTP client"),
            base_url: base_url.to_string(),
        }
    }

    /// Dispatch a quantum computation task to wqc-core
    pub async fn dispatch_task(&self, request: ComputeRequest) -> Result<ComputeResponse> {
        let url = format!("{}/compute", self.base_url);

        let response = self.client
            .post(&url)
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
}
