//! Fetches a settled task's evidence from the orchestrator (wqc-docs `spec/zk-SNARK.md` §11.6).

use std::collections::BTreeMap;

use anyhow::Context;
use reqwest::Client;
use serde::Deserialize;

use crate::domain::settlement::{self, Binding};

#[derive(Debug, Deserialize)]
struct TaskResponse {
    #[serde(default)]
    receipt_url: String,
    wrap_evidence: Option<WrapEvidence>,
}

#[derive(Debug, Deserialize)]
struct WrapEvidence {
    root_url: String,
    wrap_url: String,
    leaves_url: String,
}

/// Everything §11.6 checks a task against.
pub struct FetchedEvidence {
    pub receipt: Vec<u8>,
    pub bindings: BTreeMap<String, Binding>,
    pub root: Vec<u8>,
    pub wrap: Vec<u8>,
    pub leaves_json: Vec<u8>,
}

/// Fetches the evidence for `task_id`. Any part missing is an error: the caller treats the
/// evidence as unavailable.
pub async fn fetch(
    client: &Client,
    orch_url: &str,
    task_id: &str,
    chain_id: u64,
) -> anyhow::Result<FetchedEvidence> {
    let base = orch_url.trim_end_matches('/');
    let task: TaskResponse = client
        .get(format!("{base}/api/v1/task/{task_id}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
        .context("task response")?;
    anyhow::ensure!(!task.receipt_url.is_empty(), "no receipt_url");
    let ev = task.wrap_evidence.context("no wrap_evidence")?;
    let receipt = get(client, &task.receipt_url).await.context("receipt")?;
    let mut bindings = BTreeMap::new();
    for op in settlement::receipt_payees(&receipt).map_err(anyhow::Error::msg)? {
        let b: Binding = client
            .get(format!(
                "{base}/api/v1/economy/payout/{op}?chain_id={chain_id}"
            ))
            .send()
            .await?
            .error_for_status()
            .with_context(|| format!("payout binding of {op}"))?
            .json()
            .await?;
        bindings.insert(op, b);
    }
    Ok(FetchedEvidence {
        receipt,
        bindings,
        root: get(client, &ev.root_url).await.context("root")?,
        wrap: get(client, &ev.wrap_url).await.context("wrap chain")?,
        leaves_json: get(client, &ev.leaves_url).await.context("leaves")?,
    })
}

async fn get(client: &Client, url: &str) -> anyhow::Result<Vec<u8>> {
    Ok(client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?
        .to_vec())
}
