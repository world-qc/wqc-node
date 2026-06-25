//! HTTP discovery of orchestrator libp2p bootstrap targets.

use anyhow::Context;
use libp2p::multiaddr::Protocol;
use libp2p::{Multiaddr, PeerId};
use reqwest::Client;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
struct BootstrapResponse {
    peer_id: String,
    public_key_b64: String,
    multiaddrs: Vec<String>,
}

/// Resolved orchestrator identity and dial targets (held in memory for the process lifetime).
#[derive(Debug, Clone)]
pub struct OrchestratorBootstrap {
    pub source_url: String,
    pub peer_id: PeerId,
    pub public_key_b64: String,
    pub multiaddrs: Vec<String>,
}

pub async fn resolve_bootstrap(
    client: &Client,
    bootstrap_urls: &[String],
) -> anyhow::Result<OrchestratorBootstrap> {
    if bootstrap_urls.is_empty() {
        anyhow::bail!("WQC_BOOTSTRAP_URLS must include at least one HTTP(S) URL");
    }

    let mut last_err = None;
    for raw_url in bootstrap_urls {
        let url = raw_url.trim();
        if url.is_empty() {
            continue;
        }
        match fetch_bootstrap(client, url).await {
            Ok(bootstrap) => {
                tracing::info!(
                    "Resolved orchestrator bootstrap from {} (peer_id={})",
                    bootstrap.source_url,
                    bootstrap.peer_id
                );
                return Ok(bootstrap);
            }
            Err(e) => {
                tracing::warn!("Failed to fetch orchestrator bootstrap from {url}: {e:#}");
                last_err = Some(e);
            }
        }
    }

    Err(last_err.unwrap_or_else(|| {
        anyhow::anyhow!("no bootstrap URLs provided in WQC_BOOTSTRAP_URLS")
    }))
}

async fn fetch_bootstrap(client: &Client, endpoint: &str) -> anyhow::Result<OrchestratorBootstrap> {
    let endpoint = endpoint.trim();
    if endpoint.is_empty() {
        anyhow::bail!("bootstrap URL is empty");
    }

    let response = client
        .get(endpoint)
        .send()
        .await
        .with_context(|| format!("GET {endpoint} failed"))?
        .error_for_status()
        .with_context(|| format!("GET {endpoint} returned error status"))?;

    let payload: BootstrapResponse = response
        .json()
        .await
        .with_context(|| format!("decode bootstrap JSON from {endpoint}"))?;

    validate_bootstrap(endpoint, payload)
}

fn validate_bootstrap(
    source_url: &str,
    payload: BootstrapResponse,
) -> anyhow::Result<OrchestratorBootstrap> {
    if payload.public_key_b64.trim().is_empty() {
        anyhow::bail!("bootstrap response missing public_key_b64");
    }
    if payload.multiaddrs.is_empty() {
        anyhow::bail!("bootstrap response missing multiaddrs");
    }

    let peer_id: PeerId = payload
        .peer_id
        .parse()
        .with_context(|| format!("invalid peer_id {}", payload.peer_id))?;

    let mut multiaddrs = Vec::with_capacity(payload.multiaddrs.len());
    for raw in payload.multiaddrs {
        let addr: Multiaddr = raw
            .parse()
            .with_context(|| format!("invalid bootstrap multiaddr {raw}"))?;
        let addr_peer = peer_id_from_multiaddr(&addr)
            .with_context(|| format!("bootstrap multiaddr missing /p2p: {raw}"))?;
        if addr_peer != peer_id {
            anyhow::bail!(
                "bootstrap peer_id mismatch: field={peer_id}, multiaddr={addr_peer} ({raw})"
            );
        }
        multiaddrs.push(raw);
    }

    Ok(OrchestratorBootstrap {
        source_url: source_url.to_string(),
        peer_id,
        public_key_b64: payload.public_key_b64.trim().to_string(),
        multiaddrs,
    })
}

fn peer_id_from_multiaddr(addr: &Multiaddr) -> Option<PeerId> {
    for protocol in addr.iter() {
        if let Protocol::P2p(peer_id) = protocol {
            return Some(peer_id);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_bootstrap_checks_peer_id_consistency() {
        let peer = "12D3KooWDmYmHPsTGDi9QNvEDURikkhWoj2wWEnSjwvQeDXmhak3";
        let addr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{peer}");
        let bootstrap = validate_bootstrap(
            "http://orch:9000/api/v1/p2p/bootstrap",
            BootstrapResponse {
                peer_id: peer.to_string(),
                public_key_b64: "abc".to_string(),
                multiaddrs: vec![addr],
            },
        )
        .expect("valid bootstrap");

        assert_eq!(bootstrap.public_key_b64, "abc");
        assert_eq!(bootstrap.multiaddrs.len(), 1);
        assert_eq!(
            bootstrap.source_url,
            "http://orch:9000/api/v1/p2p/bootstrap"
        );
    }
}
