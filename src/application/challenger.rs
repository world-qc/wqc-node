//! Checks this node's settled tasks on SettlementV4 during their challenge window and
//! challenges a settlement that does not match the published evidence (wqc-docs
//! `spec/zk-SNARK.md` §11.6).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use futures::stream::{self, StreamExt};

use crate::application::state::AppState;
use crate::config::ChallengerConfig;
use crate::domain::models::Proof;
use crate::domain::settlement::{self, OwnLeaf, STATUS_SETTLED_COMMITTED};
use crate::infra::settlement_chain::SettlementChain;
use crate::infra::settlement_evidence;
use crate::infra::storage::SettleWatch;

/// A task with escrow but not settled this long after the leaf is no longer watched.
const UNSETTLED_TTL_SECS: i64 = 24 * 3600;
/// How often a task with escrow is asked whether it is settled yet.
const UNSETTLED_POLL_SECS: i64 = 60;
/// SettlementV4 status of a task without on-chain escrow: it will never settle there.
const STATUS_NONE: u8 = 0;
/// Evidence fetches give up after this long.
const FETCH_TIMEOUT: Duration = Duration::from_secs(120);

/// Records a v2 leaf once its result reached the orchestrator, so its task's settlement gets
/// checked. `wire_body` is the result as delivered; v1 leaves and failures are skipped.
pub fn watch_after_delivery(
    state: &AppState,
    orchestrator_pubkey: &str,
    sub_task_id: &str,
    wire_body: &[u8],
) {
    if state.config.challenger.is_none() {
        return;
    }
    #[derive(serde::Deserialize)]
    struct Wire {
        proof: Option<Proof>,
        #[serde(default)]
        error: Option<String>,
    }
    let Ok(Wire {
        proof: Some(proof),
        error: None,
    }) = serde_json::from_slice::<Wire>(wire_body)
    else {
        return;
    };
    let Ok(sv_digest) = <[u64; 4]>::try_from(proof.public_inputs.sv_digest.as_slice()) else {
        return; // a v1 leaf: no wrap, no settlement on SettlementV4
    };
    let parent = match state.storage.load_task(orchestrator_pubkey, sub_task_id) {
        Ok(Some(task)) => task.request.parent_task_id,
        _ => None,
    };
    let Some(parent_task_id) = parent else {
        return;
    };
    let w = SettleWatch {
        parent_task_id,
        sub_task_id: proof.public_inputs.sub_task_id,
        node_id: proof.public_inputs.node_id,
        sv_digest,
        created_at: unix_now(),
    };
    if let Err(e) = state.storage.add_settle_watch(&w) {
        tracing::warn!(
            "[Challenger] could not record sub_task_id={}: {e:#}",
            w.sub_task_id
        );
    }
}

pub fn spawn(state: Arc<AppState>, cfg: ChallengerConfig) -> anyhow::Result<()> {
    let chain = SettlementChain::new(&cfg.rpc_url, cfg.settlement, cfg.private_key.as_deref())?;
    tracing::info!(
        "[Challenger] watching SettlementV4 {} on chain {} (orchestrator {}, {})",
        cfg.settlement,
        cfg.chain_id,
        cfg.orch_url,
        if chain.can_challenge() {
            "challenges enabled"
        } else {
            "report only: no WQC_CHALLENGER_PRIVATE_KEY"
        }
    );
    tokio::spawn(async move {
        let client = match reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(FETCH_TIMEOUT)
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("[Challenger] HTTP client: {e:#}");
                return;
            }
        };
        let mut next_unsettled_check: HashMap<String, i64> = HashMap::new();
        let mut tick = tokio::time::interval(Duration::from_secs(cfg.poll_secs));
        loop {
            tick.tick().await;
            if let Err(e) = poll(&state, &cfg, &chain, &client, &mut next_unsettled_check).await {
                tracing::warn!("[Challenger] poll failed: {e:#}");
            }
        }
    });
    Ok(())
}

async fn poll(
    state: &AppState,
    cfg: &ChallengerConfig,
    chain: &SettlementChain,
    client: &reqwest::Client,
    next_unsettled_check: &mut HashMap<String, i64>,
) -> anyhow::Result<()> {
    let mut by_task: BTreeMap<String, Vec<SettleWatch>> = BTreeMap::new();
    for w in state.storage.list_settle_watch()? {
        by_task.entry(w.parent_task_id.clone()).or_default().push(w);
    }
    if by_task.is_empty() {
        return Ok(());
    }
    next_unsettled_check.retain(|task_id, _| by_task.contains_key(task_id));
    let now = chain.chain_time().await?;
    let due: Vec<_> = by_task
        .into_iter()
        .filter(|(task_id, _)| {
            next_unsettled_check
                .get(task_id)
                .is_none_or(|&at| unix_now() >= at)
        })
        .collect();
    // Tasks are independent; checking them one at a time let a slow one (evidence fetch,
    // wrap digest) hold up the others' windows.
    let checked: Vec<_> = stream::iter(due)
        .map(|(task_id, watches)| async move {
            let r = check_task(state, cfg, chain, client, &task_id, &watches, now).await;
            (task_id, r)
        })
        .buffer_unordered(cfg.concurrency)
        .collect()
        .await;
    for (task_id, r) in checked {
        match r {
            Ok(Checked::Unsettled) => {
                next_unsettled_check.insert(task_id, unix_now() + UNSETTLED_POLL_SECS);
            }
            Ok(Checked::Done) => {
                next_unsettled_check.remove(&task_id);
            }
            Err(e) => tracing::warn!("[Challenger] task {task_id}: {e:#}"),
        }
    }
    Ok(())
}

enum Checked {
    /// Escrow locked, not settled yet: ask again later.
    Unsettled,
    /// Checked, challenged, out of the window, or never to be settled: no longer watched, or
    /// looked at again on the next poll.
    Done,
}

async fn check_task(
    state: &AppState,
    cfg: &ChallengerConfig,
    chain: &SettlementChain,
    client: &reqwest::Client,
    task_id: &str,
    watches: &[SettleWatch],
    chain_now: u64,
) -> anyhow::Result<Checked> {
    let on_chain = chain.task(task_id).await?;
    if on_chain.status == STATUS_NONE {
        // Escrow is locked at submit, before any dispatch: a task without it never settles on
        // chain (testnet's Redis-only tasks).
        state.storage.drop_settle_watch(task_id)?;
        return Ok(Checked::Done);
    }
    if on_chain.status < STATUS_SETTLED_COMMITTED {
        if watches
            .iter()
            .all(|w| unix_now() - w.created_at > UNSETTLED_TTL_SECS)
        {
            state.storage.drop_settle_watch(task_id)?;
            return Ok(Checked::Done);
        }
        return Ok(Checked::Unsettled);
    }
    if on_chain.status != STATUS_SETTLED_COMMITTED || chain_now >= on_chain.deadline {
        // Challenged, finalized, reverted, or past the window: nothing left to do.
        state.storage.drop_settle_watch(task_id)?;
        return Ok(Checked::Done);
    }

    let ev = match settlement_evidence::fetch(client, &cfg.orch_url, task_id, cfg.chain_id).await {
        Ok(ev) => ev,
        Err(e) => {
            if chain_now + cfg.margin_secs < on_chain.deadline {
                tracing::warn!(
                    "[Challenger] task {task_id}: evidence unavailable, retrying: {e:#}"
                );
                return Ok(Checked::Done);
            }
            challenge(
                state,
                chain,
                task_id,
                &[format!("evidence unavailable: {e:#}")],
            )
            .await?;
            return Ok(Checked::Done);
        }
    };
    // Evidence that does not verify is no evidence: the digest check fails below.
    let wrap_digest = match recompute_wrap_digest(&ev).await {
        Ok(d) => Some(d),
        Err(e) => {
            challenge(
                state,
                chain,
                task_id,
                &[format!("wrap evidence does not verify: {e:#}")],
            )
            .await?;
            return Ok(Checked::Done);
        }
    };
    let own: Vec<OwnLeaf> = watches
        .iter()
        .map(|w| OwnLeaf {
            sub_task_id: w.sub_task_id.clone(),
            node_id: w.node_id.clone(),
            sv_digest: w.sv_digest,
        })
        .collect();
    let failed = settlement::check(
        &on_chain,
        &settlement::Evidence {
            receipt: &ev.receipt,
            bindings: &ev.bindings,
            root: &ev.root,
            leaves_json: &ev.leaves_json,
        },
        cfg.chain_id,
        state.config.operator_id.as_deref(),
        &own,
        wrap_digest,
    );
    if failed.is_empty() {
        // Scripts wait for this line: keep its wording.
        tracing::info!("[Challenger] task {task_id}: settlement matches the evidence");
        state.storage.drop_settle_watch(task_id)?;
        return Ok(Checked::Done);
    }
    challenge(state, chain, task_id, &failed).await?;
    Ok(Checked::Done)
}

async fn challenge(
    state: &AppState,
    chain: &SettlementChain,
    task_id: &str,
    failed: &[String],
) -> anyhow::Result<()> {
    for f in failed {
        tracing::error!("[Challenger] task {task_id}: {f}");
    }
    if chain.can_challenge() {
        match chain.challenge(task_id).await {
            Ok(tx) => tracing::error!("[Challenger] task {task_id}: challenged in {tx}"),
            Err(e) => {
                // Another node may have challenged first: then the task has left SettledCommitted.
                match chain.task(task_id).await {
                    Ok(t) if t.status != STATUS_SETTLED_COMMITTED => tracing::info!(
                        "[Challenger] task {task_id}: already challenged or closed (status {})",
                        t.status
                    ),
                    _ => return Err(e),
                }
            }
        }
    } else {
        tracing::error!(
            "[Challenger] task {task_id}: would challenge (no WQC_CHALLENGER_PRIVATE_KEY)"
        );
    }
    state.storage.drop_settle_watch(task_id)?;
    Ok(())
}

/// The wrap digest from the evidence's leaves, root and wrap chain: the root verified over
/// the leaves, then the wrap chain over the root (several seconds of CPU, off the runtime).
async fn recompute_wrap_digest(
    ev: &settlement_evidence::FetchedEvidence,
) -> anyhow::Result<[u64; 4]> {
    let leaves = String::from_utf8(ev.leaves_json.clone())?;
    let (root, wrap) = (ev.root.clone(), ev.wrap.clone());
    let digest = tokio::task::spawn_blocking(move || {
        wqc_sv_stark::root_input::verify_wrap_json(&leaves, &root, &wrap)
    })
    .await??;
    Ok(digest)
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
