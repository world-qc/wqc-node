use crate::application::state::AppState;
use crate::config::NodeConfig;
use crate::domain::pcs::{PcsMemoryPolicy, PcsOpenCall};

/// Returns true when this node should bid on a CAS PCS open call.
///
/// Only spill-policy cores bid. Refuse-policy cores stay silent so the
/// orchestrator never nominates a builder that will immediately memory-gate.
pub fn should_bid_open_call(
    config: &NodeConfig,
    core_policy: PcsMemoryPolicy,
    open: &PcsOpenCall,
) -> bool {
    should_bid_open_call_at(config, core_policy, open, unix_now())
}

pub fn should_bid_open_call_at(
    config: &NodeConfig,
    core_policy: PcsMemoryPolicy,
    open: &PcsOpenCall,
    now_unix: i64,
) -> bool {
    if !core_policy.is_spill() {
        return false;
    }
    if open.is_expired_at(now_unix) {
        return false;
    }
    if open.refuses_builder(&config.peer_id) {
        return false;
    }
    if open.leaf_proof_hash.is_empty() || open.sub_task_id.is_empty() {
        return false;
    }
    true
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Reason string for debug logs when skipping a bid.
pub fn skip_bid_reason(
    config: &NodeConfig,
    core_policy: PcsMemoryPolicy,
    open: &PcsOpenCall,
) -> &'static str {
    if core_policy != PcsMemoryPolicy::Spill {
        return "policy=refuse";
    }
    if open.is_expired() {
        return "expired";
    }
    if open.refuses_builder(&config.peer_id) {
        return "already_refused";
    }
    if open.leaf_proof_hash.is_empty() || open.sub_task_id.is_empty() {
        return "incomplete_open_call";
    }
    "unknown"
}

/// Probe the connected wqc-core for its prove-time PCS memory gate policy.
pub async fn fetch_core_pcs_memory_policy(state: &AppState) -> PcsMemoryPolicy {
    match state.core_client.get_system_info().await {
        Ok(info) => info.pcs_memory_policy(),
        Err(err) => {
            tracing::warn!("[PCS Open] Failed to fetch core sysinfo for pcs_memory_policy: {err}");
            PcsMemoryPolicy::Refuse
        }
    }
}

/// Cache the latest open-call announcement so a later pcs-req can fetch CAS.
pub async fn cache_open_call(state: &AppState, open: PcsOpenCall) {
    let key = open.sub_task_id.clone();
    let mut cache = state.open_call_cache.lock().await;
    cache.insert(key, open);
}

/// Lookup a cached open call for `sub_task_id`, optionally requiring hash match.
pub async fn cached_open_call(
    state: &AppState,
    sub_task_id: &str,
    leaf_proof_hash: &str,
) -> Option<PcsOpenCall> {
    let cache = state.open_call_cache.lock().await;
    let open = cache.get(sub_task_id)?;
    if !leaf_proof_hash.is_empty() && open.leaf_proof_hash != leaf_proof_hash {
        return None;
    }
    Some(open.clone())
}

/// Remove a cached open call after permanent refusal / successful delivery.
pub async fn clear_cached_open_call(state: &AppState, sub_task_id: &str) {
    let mut cache = state.open_call_cache.lock().await;
    cache.remove(sub_task_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::pcs::PcsOpenCall;
    use ed25519_dalek::SigningKey;
    use num_bigint::BigInt;

    fn test_config(peer_id: &str) -> NodeConfig {
        NodeConfig {
            peer_id: peer_id.to_string(),
            core_url: String::new(),
            max_qubits: 28,
            max_memory_gib: 16.0,
            compute_timeout_secs: 300,
            pcs_timeout_secs: 7200,
            signing_key: SigningKey::from_bytes(&[1u8; 32]),
            bootstrap_urls: vec![],
            bootstrap_source_url: None,
            bootstrap_peers: vec![],
            p2p_listen_port: 0,
            p2p_idle_timeout_secs: 60,
            http_port: 0,
            database_url: String::new(),
            stake_amount: BigInt::from(0),
            orchestrator_peer_id: None,
            orchestrator_public_key: None,
            operator_id: None,
            operator_signing_key: None,
        }
    }

    fn sample_open(deadline: i64, refused: &[&str]) -> PcsOpenCall {
        PcsOpenCall {
            parent_task_id: "parent".to_string(),
            sub_task_id: "parent_01-sub".to_string(),
            slice_id: "01".to_string(),
            leaf_proof_hash: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .to_string(),
            leaf_proof_bytes: 42,
            cas_presigned_url: "https://s3.example/blob".to_string(),
            r_pcs_planck: "100".to_string(),
            deadline_unix: deadline,
            issued_at_unix: deadline - 1800,
            refused_builders: refused.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    #[test]
    fn spill_bids_when_open_call_active() {
        let cfg = test_config("node-a");
        let open = sample_open(2_000_000_000, &[]);
        assert!(should_bid_open_call_at(
            &cfg,
            PcsMemoryPolicy::Spill,
            &open,
            1_900_000_000
        ));
    }

    #[test]
    fn refuse_policy_skips_bid() {
        let cfg = test_config("node-a");
        let open = sample_open(2_000_000_000, &[]);
        assert!(!should_bid_open_call_at(
            &cfg,
            PcsMemoryPolicy::Refuse,
            &open,
            1_900_000_000
        ));
        assert_eq!(
            skip_bid_reason(&cfg, PcsMemoryPolicy::Refuse, &open),
            "policy=refuse"
        );
    }

    #[test]
    fn expired_open_call_skips_bid() {
        let cfg = test_config("node-a");
        let open = sample_open(1_700_000_000, &[]);
        assert!(!should_bid_open_call_at(
            &cfg,
            PcsMemoryPolicy::Spill,
            &open,
            1_700_000_000
        ));
        assert!(!should_bid_open_call_at(
            &cfg,
            PcsMemoryPolicy::Spill,
            &open,
            1_700_000_001
        ));
    }

    #[test]
    fn refused_builder_skips_bid() {
        let cfg = test_config("node-a");
        let open = sample_open(2_000_000_000, &["node-a"]);
        assert!(!should_bid_open_call_at(
            &cfg,
            PcsMemoryPolicy::Spill,
            &open,
            1_900_000_000
        ));
    }
}
