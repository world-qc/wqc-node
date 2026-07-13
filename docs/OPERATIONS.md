# wqc-node Operations

This document is the operational reference for running a worker node in the WQC devnet / public-testnet style environment. It reflects the **libp2p P2P** architecture; the legacy HTTP `/submit` + webhook path is removed.

Use this guide after reading `wqc-node/README.md`. The README is the entry point; this file is the runbook.

## Quick operator checklist

Before starting a node, confirm all of the following:

- `wqc-core` is running and reachable from `WQC_CORE_URL`
- `WQC_NODE_PRIVATE_KEY` is set to a Base64-encoded 32-byte Ed25519 seed
- `WQC_BOOTSTRAP_URLS` points at at least one working orchestrator bootstrap endpoint
- `WQC_MAX_MEMORY_GB` is set to a value that makes sense for the host
- the host can accept inbound libp2p traffic on `WQC_P2P_LISTEN_PORT` if needed by your network topology
- `jq` is available locally if you plan to use the sample status commands below

## Runtime layout

| Process | Default port | Role |
| :--- | :--- | :--- |
| `wqc-node` libp2p | `4002` (`WQC_P2P_LISTEN_PORT`) | Swarm traffic (TCP + QUIC) |
| `wqc-node` HTTP | `8080` (`WQC_HTTP_PORT`) | Admin only (`/health`, `/status`) |
| `wqc-core` | `3000` or unix socket | Circuit simulation + STARK proving |
| `wqc-orchestrator` libp2p | `4001` | Bootstrap peer for nodes |

Each node process executes **one sub-task at a time**. Throughput scales by adding nodes, not by raising per-node concurrency.

## Public participation model

- The node is a **worker**, not a client entrypoint.
- Task traffic is **P2P only**.
- The HTTP server is **admin-only** and meant for local checks.
- Rewards, burns, and client balances are maintained by the orchestrator-side off-chain ledger in the current testnet model.

## Required configuration

### `WQC_NODE_PRIVATE_KEY`

- Format: standard Base64 encoding of exactly **32 bytes** (Ed25519 seed).
- Used for: libp2p PeerID, bid Ed25519 signatures.
- **One key per node** in multi-node deployments.

### `WQC_BOOTSTRAP_URLS`

- Comma-separated **full** bootstrap endpoint URLs (path included).
- At startup the node `GET`s each URL in order until one succeeds; response fields `peer_id`, `public_key_b64`, and `multiaddrs` are kept in memory.
- Use multiple URLs for **failover** (first success wins).
- Example: `http://wqc-orchestrator-01:9000/api/v1/p2p/bootstrap`
- Backup example: `http://primary:9000/api/v1/p2p/bootstrap,http://backup:9000/api/v1/p2p/bootstrap`
- On P2P disconnect, the node redials the resolved multiaddrs with exponential backoff (1s → 60s cap).

Orchestrator must expose dialable libp2p addresses. In Docker set `WQC_P2P_ADVERTISE_ADDRS` on the orchestrator (listen bind `0.0.0.0` is not dialable from other containers).

### `WQC_CORE_URL`

- HTTP: `http://wqc-core-02:3000` or `http://localhost:3000`
- Unix domain socket (Linux): `unix:/var/run/wqc-core-01.sock` (requires `reqwest` unix feature; used in compose sidecar layout)

Core must expose `GET /gates`, `GET /sysinfo`, and `POST /compute`.

## Recommended minimal configuration

For most participants, these are the only variables you need to touch first:

```bash
export WQC_NODE_PRIVATE_KEY="<base64-32-byte-seed>"
export WQC_BOOTSTRAP_URLS="http://localhost:9000/api/v1/p2p/bootstrap"
export WQC_CORE_URL="http://localhost:3000"
export WQC_MAX_MEMORY_GB="1"
export WQC_NODE_STAKE_WQC="0.05"
```

Then start the node:

```bash
cargo run --release
```

## Optional configuration

| Variable | Default | Notes |
| :--- | :--- | :--- |
| `WQC_NODE_STAKE_WQC` | `0.05` | Parsed to Planck (pWQC) for bid `stake_amount`. Up to 18 fractional digits. |
| `WQC_TESTNET_NODE_KEY` | — | **Public testnet:** Node Key from the dashboard (`nk_…`). Derives operator identity and `operator_sig` on bids. Without it, bids are rejected. |
| `WQC_MAX_MEMORY_GB` | `16` | WQC memory budget (GiB), capped at **host total − reserve** (1 GiB if host < 16 GiB, else 2 GiB; see `memory_budget.rs`). Derives max qubits (`2^n × 16` bytes). Nodes with `< 10` max qubits never bid (`NETWORK_MIN_QUBITS`). |
| `WQC_COMPUTE_TIMEOUT_SECS` | `300` | Per-request timeout to core. |
| `WQC_DATABASE_URL` | `sqlite:wqc-node.db` | Relative path is under the process working directory. |
| `WQC_P2P_LISTEN_PORT` | `4002` | Bind `0.0.0.0` on TCP and QUIC. |
| `WQC_HTTP_PORT` | `8080` | Admin API only. |
| `WQC_RESULT_RETRY_INTERVAL_SECS` | `5` | Interval for background P2P result outbox retries. |
| `RUST_LOG` | — | e.g. `info` or `wqc_node=debug` |

### Memory budget notes

`WQC_MAX_MEMORY_GB` is the main participant-facing sizing knob.

- The node caps the requested budget at **host physical RAM minus a reserve** (1 GiB on hosts under 16 GiB total, otherwise 2 GiB) — not a flat 80% rule
- It then derives `max_qubits` from the dense envelope `2^n × 16` bytes
- That derived value is what the node advertises to the orchestrator

Examples:

- `1 GiB` budget → about `26` qubits
- `16 GiB` budget → about `30` qubits

If your node never bids, an overly small effective memory budget is one of the first things to check.

Signoff drill (devnet multi-node scrape): `wqc-docs/examples/e2e/signoff/06_memory_budget.sh`.

## Lifecycle

### Startup

1. Load env → derive libp2p PeerID from `WQC_NODE_PRIVATE_KEY`.
2. `GET` first reachable URL from `WQC_BOOTSTRAP_URLS` → cache peer id, pubkey, multiaddrs.
3. Open SQLite; re-queue any `pending` tasks.
4. Sync supported gates from `wqc-core` `GET /gates`.
5. Start libp2p host: dial bootstrap, subscribe to gossip, accept stream protocols.
6. Start result outbox retry loop (`WQC_RESULT_RETRY_INTERVAL_SECS`, default 5s).
7. Start single worker loop and admin HTTP server.

### Healthy startup signals

Look for:

- successful bootstrap resolution
- successful `GET /gates` against `wqc-core`
- `P2P host started`
- `Connected to peer`

If those appear, the node is usually ready to receive announcements and submit bids.

### Task flow

1. **Announce** — Gossip or `/wqc/task-announce/1.0.0` delivers a signed `TaskAnnouncement` envelope.
2. **Bid** — If `required_features` and qubit caps match, mine lottery proof and send signed `Bid` on `/wqc/tensor-net/1.0.0`.
3. **Dispatch** — Orchestrator pushes signed `SubTask` on `/wqc/tensor-dispatch/1.0.0` (pinned orchestrator PeerID + Ed25519 signature check).
4. **Execute** — Validate circuit → persist SQLite → `POST /compute` to core.
5. **Result** — Build P2P wire body → upsert `pending_results` → deliver on `/wqc/tensor-result/1.0.0` → delete row on ACK.
6. **Retry** — Background loop re-sends any remaining `pending_results` rows.

### Crash recovery

| State | On restart |
| :--- | :--- |
| `tasks.status = pending` | Re-enqueued for compute |
| `pending_results` row present | Retry loop delivers when P2P is ready (no re-compute) |

Completed/failed `tasks` rows are not pruned automatically.

Devnet signoff drill: `wqc-docs/examples/e2e/signoff/03_node_restart.sh` (records `/status` `pending_tasks` / `outbox_pending` around `docker restart`).

## First checks after startup

```bash
curl -s "http://localhost:8080/health"
curl -s "http://localhost:8080/status" | jq .
```

Pay attention to:

- `max_qubits`
- `max_memory_gib`
- `pending_tasks`
- `outbox_pending`
- core sysinfo visibility

## Docker compose (devnet)

Reference: `world-qc-docker/devnet/compose.yml`

- Five nodes (`wqc-node-01` … `05`), each with a unique `WQC_NODE_PRIVATE_KEY` and SQLite file.
- Shared bootstrap URL `http://wqc-orchestrator-01:9000/api/v1/p2p/bootstrap`; orchestrator advertises P2P on `10.20.3.11:4001`.
- `WQC_NODE_STAKE_WQC=0.05` on all nodes.
- `WQC_MAX_MEMORY_GB=1` is a reasonable small-host dev/test setting.

Rebuild after code changes:

```bash
docker compose -f world-qc-docker/devnet/compose.yml build wqc-node-01
docker compose -f world-qc-docker/devnet/compose.yml up -d wqc-node-01
```

## Health checks

```bash
# Admin API
curl -s "http://localhost:8080/health"
curl -s "http://localhost:8080/status" | jq .

# Logs: confirm P2P listen + orchestrator connect
#   P2P host started (peer_id=..., listen_port=4002)
#   [P2P] Connected to peer <orchestrator-peer-id>
```

Useful interpretations:

- `health=UP` but no bids: usually capability mismatch, bootstrap issues, or no incoming tasks
- rising `outbox_pending`: compute succeeded but result delivery is retrying
- rising `pending_tasks`: core is slow, stuck, or timing out

## Economics (orchestrator-side)

The node only declares stake in bids. Balance, rewards, and burns are handled by the orchestrator Redis ledger. See `wqc-orchestrator/docs/ECONOMICS.md`.

For public participants, this means:

- you do **not** need an on-chain wallet flow to run a node today
- stake is an advertised bid parameter, not a self-custodied on-chain lock
- settlement and faucet behavior are currently testnet services operated on the orchestrator side

| Concern | Where |
| :--- | :--- |
| Bid `stake_amount` | `WQC_NODE_STAKE_WQC` on node |
| Dev faucet / auto stake cap | `WQC_MAX_DEV_STAKE_WQC` on orchestrator |
| Gas from execution | `work_report` in P2P result → orchestrator `Gas_quantum` |

## Troubleshooting

| Symptom | Likely cause |
| :--- | :--- |
| `WQC_BOOTSTRAP_URLS is required` | Env not set in container/shell |
| `failed to resolve orchestrator P2P bootstrap` | Orchestrator HTTP down, or missing `WQC_P2P_ADVERTISE_ADDRS` |
| Node never bids | `supported_features` mismatch, derived `max_qubits` &lt; 10, or announcement qubits above capability |
| `[P2P Dispatch] Rejected subtask from unauthorized peer` | Dispatch from non-bootstrap PeerID |
| `failed to mine lottery proof within time window` | `bid_difficulty` too high for 10s window |
| `wqc-core returned error status` | Core down, timeout (`WQC_COMPUTE_TIMEOUT_SECS`), or OOM on large circuits |
| Pending tasks stuck after restart | Expected until core completes; check core logs |
| Result delivery failed | P2P disconnect; row kept in `pending_results` and **retried automatically** (check `GET /status` → `outbox_pending`) |

### Recommended triage order

1. Check `GET /health`
2. Check `GET /status`
3. Check `wqc-node` logs for bootstrap / P2P errors
4. Check `wqc-core` logs for compute or memory failures
5. Verify the bootstrap endpoint by calling `WQC_BOOTSTRAP_URLS` directly

### Common public-testnet mistakes

- Using a bootstrap URL that is reachable over HTTP but returns undialable libp2p addresses
- Setting `WQC_MAX_MEMORY_GB` higher than the host can realistically sustain
- Pointing `WQC_CORE_URL` at a core instance that is up but missing expected endpoints
- Reusing the same node private key across multiple node processes

## Related docs

- [wqc-orchestrator README — P2P protocols](../../wqc-orchestrator/README.md#p2p-protocols-node-facing)
- [wqc-orchestrator ECONOMICS](../../wqc-orchestrator/docs/ECONOMICS.md)
- [whitepaper_gap.md](../whitepaper_gap.md) — WP vs implementation gaps
