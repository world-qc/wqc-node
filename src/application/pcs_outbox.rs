use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::application::pcs_open_call::{cached_open_call, clear_cached_open_call};
use crate::application::state::AppState;
use crate::domain::models::{ComputeTask, Proof, PublicInputs};
use crate::domain::pcs::{OpenCallPcsSource, PcsRequest, PendingPcsJob};
use crate::infra::cas_client;
use crate::transport::p2p::pcs_delivery::{
    build_pcs_refusal_wire_body, build_pcs_wire_body, send_pcs_wire,
};

const DEFAULT_RETRY_INTERVAL_SECS: u64 = 30;
/// Jobs the orchestrator never asked for are dropped after this long: the node
/// lost the proof-winner draw, so its retained proof is dead weight.
const DEFAULT_UNREQUESTED_TTL_SECS: i64 = 6 * 3600;
/// Window for the retained proof to appear when a request beats the result ACK path.
const RETAINED_PROOF_LOOKUP_ATTEMPTS: u32 = 10;
const RETAINED_PROOF_LOOKUP_DELAY: Duration = Duration::from_millis(500);

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

fn is_core_down_error(err: &anyhow::Error) -> bool {
    let msg = format!("{err:#}");
    msg.contains("unhealthy (backoff)")
        || msg.contains("health check failed")
        || msg.contains("Failed to send leaf_pcs request to wqc-core")
}

/// PCS memory gate refuse (422): permanent; orch should compose-time fallback.
fn is_pcs_memory_refuse(err: &anyhow::Error) -> bool {
    let msg = format!("{err:#}");
    msg.contains("PCS memory:") && msg.contains("policy=refuse")
}

fn is_cas_hash_mismatch(err: &anyhow::Error) -> bool {
    format!("{err:#}").contains("CAS leaf proof hash mismatch")
}

fn pcs_refuse_reason(err: &anyhow::Error) -> String {
    let msg = format!("{err:#}");
    msg.lines()
        .find(|l| l.contains("PCS memory:") || l.contains("CAS leaf proof hash mismatch"))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("PCS memory: refused")
        .to_string()
}

fn placeholder_proof(sub_task_id: &str, node_id: &str, slice_id: &str) -> Proof {
    Proof {
        public_inputs: PublicInputs {
            circuit_id: String::new(),
            sub_task_id: sub_task_id.to_string(),
            node_id: node_id.to_string(),
            slice_id: slice_id.to_string(),
            output_result_hash: String::new(),
            measurement_spec_hash: String::new(),
        },
        stark_proof_b64: String::new(),
    }
}

/// Jobs currently running build-or-deliver for a sub_task.
/// Without this, the retry loop starts a second prove while the first is in flight.
static IN_FLIGHT: std::sync::OnceLock<Mutex<HashSet<String>>> = std::sync::OnceLock::new();

fn in_flight() -> &'static Mutex<HashSet<String>> {
    IN_FLIGHT.get_or_init(|| Mutex::new(HashSet::new()))
}

fn claim_inflight(sub_task_id: &str) -> bool {
    let mut set = in_flight().lock().expect("pcs in_flight lock");
    set.insert(sub_task_id.to_string())
}

fn release_inflight(sub_task_id: &str) {
    let mut set = in_flight().lock().expect("pcs in_flight lock");
    set.remove(sub_task_id);
}

/// Retains the proof a leaf PCS would bind to after a successful result ACK.
///
/// Proving is deliberately not started here. Only one node per slice — the
/// proof winner the orchestrator picks at quorum — is asked for a bundle, and
/// building one is as memory-hungry as the compute itself. The job sits idle
/// until [`handle_pcs_request`] arrives, or is pruned once it clearly lost.
pub fn retain_after_result(state: Arc<AppState>, task: &ComputeTask, proof: &Proof) {
    let orchestrator_pubkey = task.orchestrator_pubkey.clone();
    let sub_task_id = task.request.task_id.clone();
    let mut job = PendingPcsJob {
        sub_task_id: sub_task_id.clone(),
        proof: proof.clone(),
        requested: false,
        leaf_pcs_b64: None,
        leaf_pcs_bytes: None,
        open_call: None,
    };
    // The result retry path can re-enter; never drop a won request or cached build.
    if let Ok(Some(entry)) = state
        .storage
        .get_pending_pcs(&orchestrator_pubkey, &sub_task_id)
    {
        if let Ok(existing) = serde_json::from_slice::<PendingPcsJob>(&entry.job_json) {
            job.requested = existing.requested;
            job.open_call = existing.open_call;
            if existing.leaf_pcs_b64.is_some() {
                job.leaf_pcs_b64 = existing.leaf_pcs_b64;
                job.leaf_pcs_bytes = existing.leaf_pcs_bytes;
            }
        }
    }
    let already_requested = job.requested;

    if let Err(e) = state
        .storage
        .upsert_pending_pcs(&orchestrator_pubkey, &sub_task_id, &job)
    {
        tracing::error!(
            "[PCS Outbox] Failed to retain proof for sub_task_id={}: {}",
            sub_task_id,
            e
        );
        return;
    }

    if !already_requested {
        tracing::debug!(
            "[PCS Outbox] Retained proof for sub_task_id={}; awaiting orchestrator PCS request",
            sub_task_id
        );
        return;
    }

    spawn_build_and_deliver(state, orchestrator_pubkey, sub_task_id, job);
}

/// Starts the leaf PCS build for a sub-task this node has been named winner of,
/// or (open_call) the nominated CAS spill builder.
pub fn handle_pcs_request(state: Arc<AppState>, orchestrator_pubkey: &str, request: &PcsRequest) {
    if request.is_open_call() {
        handle_open_call_pcs_request(state, orchestrator_pubkey, request);
        return;
    }

    let orchestrator_pubkey = orchestrator_pubkey.to_string();
    let sub_task_id = request.sub_task_id.clone();

    tokio::spawn(async move {
        let mut job = None;
        for attempt in 0..RETAINED_PROOF_LOOKUP_ATTEMPTS {
            match state
                .storage
                .get_pending_pcs(&orchestrator_pubkey, &sub_task_id)
            {
                Ok(Some(entry)) => match serde_json::from_slice::<PendingPcsJob>(&entry.job_json) {
                    Ok(found) => {
                        job = Some(found);
                        break;
                    }
                    Err(e) => {
                        tracing::error!(
                            "[PCS Outbox] Corrupt retained job for sub_task_id={}: {}",
                            sub_task_id,
                            e
                        );
                        return;
                    }
                },
                Ok(None) => {}
                Err(e) => {
                    tracing::error!(
                        "[PCS Outbox] Failed to load retained proof for sub_task_id={}: {}",
                        sub_task_id,
                        e
                    );
                    return;
                }
            }
            if attempt + 1 < RETAINED_PROOF_LOOKUP_ATTEMPTS {
                tokio::time::sleep(RETAINED_PROOF_LOOKUP_DELAY).await;
            }
        }

        let Some(mut job) = job else {
            tracing::warn!(
                "[PCS Outbox] PCS requested for sub_task_id={} but no proof is retained",
                sub_task_id
            );
            return;
        };

        if !job.requested {
            job.requested = true;
            if let Err(e) =
                state
                    .storage
                    .upsert_pending_pcs(&orchestrator_pubkey, &sub_task_id, &job)
            {
                tracing::error!(
                    "[PCS Outbox] Failed to mark sub_task_id={} as requested: {}",
                    sub_task_id,
                    e
                );
                return;
            }
            tracing::info!(
                "[PCS Outbox] Named slice proof winner for sub_task_id={}; building leaf PCS",
                sub_task_id
            );
        }

        spawn_build_and_deliver(state, orchestrator_pubkey, sub_task_id, job);
    });
}

fn handle_open_call_pcs_request(
    state: Arc<AppState>,
    orchestrator_pubkey: &str,
    request: &PcsRequest,
) {
    let orchestrator_pubkey = orchestrator_pubkey.to_string();
    let sub_task_id = request.sub_task_id.clone();
    let leaf_proof_hash = request.leaf_proof_hash.clone();
    let slice_id = request.slice_id.clone();

    tokio::spawn(async move {
        if leaf_proof_hash.is_empty() {
            tracing::warn!(
                "[PCS Outbox] Open-call PCS request missing leaf_proof_hash for sub_task_id={}",
                sub_task_id
            );
            return;
        }

        let Some(open) = cached_open_call(&state, &sub_task_id, &leaf_proof_hash).await else {
            tracing::warn!(
                "[PCS Outbox] Open-call PCS request for sub_task_id={} hash={} but no matching cached announce",
                sub_task_id,
                leaf_proof_hash
            );
            return;
        };

        let job = PendingPcsJob {
            sub_task_id: sub_task_id.clone(),
            proof: placeholder_proof(&sub_task_id, &state.config.peer_id, &slice_id),
            requested: true,
            leaf_pcs_b64: None,
            leaf_pcs_bytes: None,
            open_call: Some(OpenCallPcsSource {
                leaf_proof_hash: open.leaf_proof_hash.clone(),
                cas_presigned_url: open.cas_presigned_url.clone(),
                leaf_proof_bytes: open.leaf_proof_bytes,
                slice_id: if slice_id.is_empty() {
                    open.slice_id.clone()
                } else {
                    slice_id
                },
            }),
        };

        if let Err(e) = state
            .storage
            .upsert_pending_pcs(&orchestrator_pubkey, &sub_task_id, &job)
        {
            tracing::error!(
                "[PCS Outbox] Failed to store open-call PCS job for sub_task_id={}: {}",
                sub_task_id,
                e
            );
            return;
        }

        tracing::info!(
            "[PCS Outbox] Nominated open-call builder for sub_task_id={}; fetching CAS leaf proof",
            sub_task_id
        );
        spawn_build_and_deliver(state, orchestrator_pubkey, sub_task_id, job);
    });
}

fn spawn_build_and_deliver(
    state: Arc<AppState>,
    orchestrator_pubkey: String,
    sub_task_id: String,
    job: PendingPcsJob,
) {
    tokio::spawn(async move {
        match try_build_and_deliver(state, &orchestrator_pubkey, &sub_task_id, job).await {
            Ok(DeliverOutcome::Delivered) => {}
            Ok(DeliverOutcome::SkippedInFlight) => {
                tracing::debug!(
                    "[PCS Outbox] Build skipped sub_task_id={} (already in flight)",
                    sub_task_id
                );
            }
            Ok(DeliverOutcome::SkippedCoreDown) => {
                tracing::warn!(
                    "[PCS Outbox] PCS prove deferred for sub_task_id={} (wqc-core unhealthy)",
                    sub_task_id
                );
            }
            Ok(DeliverOutcome::RefusedPermanent) => {
                tracing::info!(
                    "[PCS Outbox] PCS permanently refused for sub_task_id={}; orch will fail over / reopen",
                    sub_task_id
                );
            }
            Err(e) => {
                tracing::warn!(
                    "[PCS Outbox] PCS delivery failed for sub_task_id={} (queued for retry): {}",
                    sub_task_id,
                    e
                );
            }
        }
    });
}

#[derive(Debug, PartialEq, Eq)]
enum DeliverOutcome {
    Delivered,
    SkippedInFlight,
    /// Core is in health-gate backoff; prove deferred (does not bump attempts).
    SkippedCoreDown,
    /// Permanent refuse (memory gate or CAS hash mismatch); refusal sent, job cleared.
    RefusedPermanent,
}

async fn try_build_and_deliver(
    state: Arc<AppState>,
    orchestrator_pubkey: &str,
    sub_task_id: &str,
    mut job: PendingPcsJob,
) -> anyhow::Result<DeliverOutcome> {
    if !claim_inflight(sub_task_id) {
        return Ok(DeliverOutcome::SkippedInFlight);
    }

    let result = async {
        let (leaf_pcs_b64, bytes) = if let Some(cached) = job.leaf_pcs_b64.clone() {
            let bytes = job.leaf_pcs_bytes.unwrap_or(0);
            tracing::info!(
                "[PCS Outbox] Reusing cached leaf PCS for sub_task_id={} ({} bytes); skipping core prove",
                sub_task_id,
                bytes
            );
            (cached, bytes)
        } else {
            // Avoid hammering a dead core; cached PCS delivery still proceeds above.
            if state.core_client.is_in_backoff() {
                return Ok(DeliverOutcome::SkippedCoreDown);
            }

            let pcs = if let Some(open) = job.open_call.clone() {
                match build_open_call_leaf_pcs(state.clone(), sub_task_id, &open).await {
                    Ok(pcs) => pcs,
                    Err(e) if is_core_down_error(&e) => {
                        return Ok(DeliverOutcome::SkippedCoreDown);
                    }
                    Err(e) if is_pcs_memory_refuse(&e) || is_cas_hash_mismatch(&e) => {
                        return report_permanent_refusal(
                            state.clone(),
                            orchestrator_pubkey,
                            sub_task_id,
                            &e,
                        )
                        .await;
                    }
                    Err(e) => return Err(e),
                }
            } else {
                match state.core_client.build_leaf_pcs(job.proof.clone()).await {
                    Ok(pcs) => pcs,
                    Err(e) if is_core_down_error(&e) => {
                        return Ok(DeliverOutcome::SkippedCoreDown);
                    }
                    Err(e) if is_pcs_memory_refuse(&e) => {
                        return report_permanent_refusal(
                            state.clone(),
                            orchestrator_pubkey,
                            sub_task_id,
                            &e,
                        )
                        .await;
                    }
                    Err(e) => return Err(e),
                }
            };

            job.leaf_pcs_b64 = Some(pcs.leaf_pcs_b64.clone());
            job.leaf_pcs_bytes = Some(pcs.bytes);
            // Persist before P2P send so a delivery failure does not force a re-prove.
            state
                .storage
                .upsert_pending_pcs(orchestrator_pubkey, sub_task_id, &job)?;
            tracing::info!(
                "[PCS Outbox] Cached leaf PCS for sub_task_id={} ({} bytes)",
                sub_task_id,
                pcs.bytes
            );
            (pcs.leaf_pcs_b64, pcs.bytes)
        };

        let wire = build_pcs_wire_body(sub_task_id, &state.config.peer_id, &leaf_pcs_b64)?;
        send_pcs_wire(state.clone(), &wire).await?;
        state
            .storage
            .delete_pending_pcs(orchestrator_pubkey, sub_task_id)?;
        if job.is_open_call() {
            clear_cached_open_call(&state, sub_task_id).await;
        }
        tracing::info!(
            "[PCS Outbox] Delivered leaf PCS for sub_task_id={} ({} bytes)",
            sub_task_id,
            bytes
        );
        Ok(DeliverOutcome::Delivered)
    }
    .await;

    release_inflight(sub_task_id);
    result
}

async fn build_open_call_leaf_pcs(
    state: Arc<AppState>,
    sub_task_id: &str,
    open: &OpenCallPcsSource,
) -> anyhow::Result<crate::infra::core_client::LeafPcsResponse> {
    tracing::info!(
        "[PCS Outbox] Fetching CAS leaf proof for sub_task_id={} hash={}",
        sub_task_id,
        open.leaf_proof_hash
    );
    let proof_bytes = cas_client::fetch_and_verify(
        &state.http_client,
        &open.cas_presigned_url,
        &open.leaf_proof_hash,
        if open.leaf_proof_bytes > 0 {
            Some(open.leaf_proof_bytes)
        } else {
            None
        },
    )
    .await?;

    tracing::info!(
        "[PCS Outbox] CAS leaf proof verified for sub_task_id={} ({} bytes); calling /leaf_pcs",
        sub_task_id,
        proof_bytes.len()
    );

    state
        .core_client
        .build_leaf_pcs_from_proof_bytes(
            &proof_bytes,
            sub_task_id,
            &state.config.peer_id,
            &open.slice_id,
        )
        .await
}

async fn report_permanent_refusal(
    state: Arc<AppState>,
    orchestrator_pubkey: &str,
    sub_task_id: &str,
    err: &anyhow::Error,
) -> anyhow::Result<DeliverOutcome> {
    let reason = pcs_refuse_reason(err);
    let wire = build_pcs_refusal_wire_body(sub_task_id, &state.config.peer_id, &reason)?;
    send_pcs_wire(state.clone(), &wire).await?;
    state
        .storage
        .delete_pending_pcs(orchestrator_pubkey, sub_task_id)?;
    clear_cached_open_call(&state, sub_task_id).await;
    tracing::warn!(
        "[PCS Outbox] Reported PCS refusal for sub_task_id={}: {}",
        sub_task_id,
        reason
    );
    Ok(DeliverOutcome::RefusedPermanent)
}

pub fn spawn_retry_loop(state: Arc<AppState>) {
    let interval_secs = std::env::var("WQC_PCS_RETRY_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_RETRY_INTERVAL_SECS);
    let unrequested_ttl_secs = std::env::var("WQC_PCS_UNREQUESTED_TTL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_UNREQUESTED_TTL_SECS);

    tokio::spawn(async move {
        let interval = Duration::from_secs(interval_secs);
        tracing::info!(
            "[PCS Outbox] Retry loop started (interval={}s, unrequested_ttl={}s)",
            interval_secs,
            unrequested_ttl_secs
        );

        loop {
            tokio::time::sleep(interval).await;

            let pending = match state.storage.list_pending_pcs() {
                Ok(entries) => entries,
                Err(e) => {
                    tracing::error!("[PCS Outbox] Failed to list pending PCS jobs: {}", e);
                    continue;
                }
            };

            if pending.is_empty() {
                continue;
            }

            tracing::debug!("[PCS Outbox] Retrying {} pending PCS job(s)", pending.len());

            let now = unix_now();
            for entry in pending {
                let job: PendingPcsJob = match serde_json::from_slice(&entry.job_json) {
                    Ok(j) => j,
                    Err(e) => {
                        tracing::error!(
                            "[PCS Outbox] Corrupt job for sub_task_id={}: {}",
                            entry.sub_task_id,
                            e
                        );
                        continue;
                    }
                };

                if !job.requested {
                    if now - entry.created_at >= unrequested_ttl_secs {
                        if let Err(e) = state.storage.delete_pending_pcs_by_id(entry.id) {
                            tracing::error!(
                                "[PCS Outbox] Failed to prune unrequested sub_task_id={}: {}",
                                entry.sub_task_id,
                                e
                            );
                        } else {
                            tracing::info!(
                                "[PCS Outbox] Pruned unrequested sub_task_id={} (not the slice proof winner)",
                                entry.sub_task_id
                            );
                        }
                    }
                    continue;
                }

                match try_build_and_deliver(
                    state.clone(),
                    &entry.orchestrator_pubkey,
                    &entry.sub_task_id,
                    job,
                )
                .await
                {
                    Ok(DeliverOutcome::Delivered) => {
                        tracing::info!(
                            "[PCS Outbox] Retry delivered sub_task_id={} after {} attempt(s)",
                            entry.sub_task_id,
                            entry.attempts + 1
                        );
                    }
                    Ok(DeliverOutcome::SkippedInFlight) => {
                        tracing::debug!(
                            "[PCS Outbox] Retry skipped sub_task_id={} (build/deliver already in flight)",
                            entry.sub_task_id
                        );
                    }
                    Ok(DeliverOutcome::SkippedCoreDown) => {
                        tracing::debug!(
                            "[PCS Outbox] Retry deferred sub_task_id={} (wqc-core unhealthy)",
                            entry.sub_task_id
                        );
                    }
                    Ok(DeliverOutcome::RefusedPermanent) => {
                        tracing::info!(
                            "[PCS Outbox] PCS refusal delivered for sub_task_id={}",
                            entry.sub_task_id
                        );
                    }
                    Err(e) => {
                        if let Err(inc_err) = state.storage.increment_pending_pcs_attempts(entry.id)
                        {
                            tracing::error!(
                                "[PCS Outbox] Failed to increment attempts for sub_task_id={}: {}",
                                entry.sub_task_id,
                                inc_err
                            );
                        }
                        tracing::warn!(
                            "[PCS Outbox] Retry deliver failed for sub_task_id={}: {}",
                            entry.sub_task_id,
                            e
                        );
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::cas_client::sha256_hex;

    #[test]
    fn cas_hash_mismatch_is_permanent_refuse_reason() {
        let want = sha256_hex(b"a");
        let err = cas_client::verify_cas_blob(b"b", &want).unwrap_err();
        assert!(is_cas_hash_mismatch(&err));
        let reason = pcs_refuse_reason(&err);
        assert!(reason.contains("hash mismatch"));
    }

    #[test]
    fn open_call_job_round_trips_source() {
        let job = PendingPcsJob {
            sub_task_id: "s".into(),
            proof: placeholder_proof("s", "n", "01"),
            requested: true,
            leaf_pcs_b64: None,
            leaf_pcs_bytes: None,
            open_call: Some(OpenCallPcsSource {
                leaf_proof_hash: "aa".into(),
                cas_presigned_url: "https://x".into(),
                leaf_proof_bytes: 3,
                slice_id: "01".into(),
            }),
        };
        let json = serde_json::to_vec(&job).unwrap();
        let back: PendingPcsJob = serde_json::from_slice(&json).unwrap();
        assert!(back.is_open_call());
        assert_eq!(back.open_call.as_ref().unwrap().leaf_proof_bytes, 3);
    }
}
