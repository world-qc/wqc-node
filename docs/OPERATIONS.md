# wqc-node Operations

This document is the operational reference for running a worker node in the WQC devnet/PoC. It reflects the **libp2p P2P** architecture (the legacy HTTP `/submit` + webhook path is removed).

## Runtime layout

| Process | Default port | Role |
| :--- | :--- | :--- |
| `wqc-node` libp2p | `4002` (`WQC_P2P_LISTEN_PORT`) | Swarm traffic (TCP + QUIC) |
| `wqc-node` HTTP | `8080` (`WQC_HTTP_PORT`) | Admin only (`/health`, `/status`) |
| `wqc-core` | `3000` or unix socket | Circuit simulation + STARK proving |
| `wqc-orchestrator` libp2p | `4001` | Bootstrap peer for nodes |

Each node process executes **one sub-task at a time**. Throughput scales by adding nodes, not by raising per-node concurrency.

## Required configuration

### `WQC_NODE_PRIVATE_KEY`

- Format: standard Base64 encoding of exactly **32 bytes** (Ed25519 seed).
- Used for: libp2p PeerID, bid Ed25519 signatures.
- **One key per node** in multi-node deployments.

### `WQC_ORCHESTRATOR_BOOTSTRAP`

- Comma-separated [libp2p multiaddr](https://github.com/multiformats/multiaddr) list.
- Must include `/p2p/<orchestrator-peer-id>` so the node can pin the orchestrator PeerID.
- Example: `/ip4/10.20.3.11/tcp/4001/p2p/12D3KooWDmYmHPsTGDi9QNvEDURikkhWoj2wWEnSjwvQeDXmhak3`
- On disconnect, the node redials with exponential backoff (1s → 60s cap).

### `WQC_ORCHESTRATOR_PUBLIC_KEY`

- Base64 Ed25519 **public** key of the orchestrator (32-byte raw key, not PEM).
- Stored with each `ComputeTask` in SQLite as the task owner identity.
- Must match the orchestrator instance you bootstrap to.

### `WQC_CORE_URL`

- HTTP: `http://wqc-core-02:3000` or `http://localhost:3000`
- Unix domain socket (Linux): `unix:/var/run/wqc-core-01.sock` (requires `reqwest` unix feature; used in compose sidecar layout)

Core must expose `GET /gates`, `GET /sysinfo`, and `POST /compute`.

## Optional configuration

| Variable | Default | Notes |
| :--- | :--- | :--- |
| `WQC_NODE_STAKE_WQC` | `0.05` | Parsed to Planck (pWQC) for bid `stake_amount`. Up to 18 fractional digits. |
| `WQC_MAX_QUBITS` | `30` | Sub-tasks above this are rejected. Nodes with `< 10` max qubits never bid (`NETWORK_MIN_QUBITS`). |
| `WQC_COMPUTE_TIMEOUT_SECS` | `300` | Per-request timeout to core. |
| `WQC_DATABASE_URL` | `sqlite:wqc-node.db` | Relative path is under the process working directory. |
| `WQC_P2P_LISTEN_PORT` | `4002` | Bind `0.0.0.0` on TCP and QUIC. |
| `WQC_HTTP_PORT` | `8080` | Admin API only. |
| `WQC_RESULT_RETRY_INTERVAL_SECS` | `5` | Interval for background P2P result outbox retries. |
| `RUST_LOG` | — | e.g. `info` or `wqc_node=debug` |

## Lifecycle

### Startup

1. Load env → derive libp2p PeerID from `WQC_NODE_PRIVATE_KEY`.
2. Open SQLite; re-queue any `pending` tasks.
3. Sync supported gates from `wqc-core` `GET /gates`.
4. Start libp2p host: dial bootstrap, subscribe to gossip, accept stream protocols.
5. Start result outbox retry loop (`WQC_RESULT_RETRY_INTERVAL_SECS`, default 5s).
6. Start single worker loop and admin HTTP server.

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

## Docker compose (devnet)

Reference: `world-qc-docker/wqc/compose.yml`

- Five nodes (`wqc-node-01` … `05`), each with a unique `WQC_NODE_PRIVATE_KEY` and SQLite file.
- Shared orchestrator bootstrap on `10.20.3.11:4001`.
- `WQC_NODE_STAKE_WQC=0.05` on all nodes.

Rebuild after code changes:

```bash
docker compose -f world-qc-docker/wqc/compose.yml build wqc-node-01
docker compose -f world-qc-docker/wqc/compose.yml up -d wqc-node-01
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

## Economics (orchestrator-side)

The node only declares stake in bids. Balance, rewards, and burns are handled by the orchestrator Redis ledger. See `wqc-orchestrator/docs/ECONOMICS.md`.

| Concern | Where |
| :--- | :--- |
| Bid `stake_amount` | `WQC_NODE_STAKE_WQC` on node |
| Dev faucet / auto stake cap | `WQC_MAX_DEV_STAKE_WQC` on orchestrator |
| Gas from execution | `work_report` in P2P result → orchestrator `Gas_quantum` |

## Troubleshooting

| Symptom | Likely cause |
| :--- | :--- |
| `WQC_ORCHESTRATOR_BOOTSTRAP is required` | Env not set in container/shell |
| `WQC_ORCHESTRATOR_PUBLIC_KEY is required` | Missing orchestrator pubkey |
| Node never bids | `supported_features` mismatch, `WQC_MAX_QUBITS` &lt; 10, or announcement qubits above capability |
| `[P2P Dispatch] Rejected subtask from unauthorized peer` | Dispatch from non-bootstrap PeerID |
| `failed to mine lottery proof within time window` | `bid_difficulty` too high for 10s window |
| `wqc-core returned error status` | Core down, timeout (`WQC_COMPUTE_TIMEOUT_SECS`), or OOM on large circuits |
| Pending tasks stuck after restart | Expected until core completes; check core logs |
| Result delivery failed | P2P disconnect; row kept in `pending_results` and **retried automatically** (check `GET /status` → `outbox_pending`) |

## Related docs

- [wqc-orchestrator README — P2P protocols](../../wqc-orchestrator/README.md#p2p-protocols-node-facing)
- [wqc-orchestrator ECONOMICS](../../wqc-orchestrator/docs/ECONOMICS.md)
- [whitepaper_gap.md](../whitepaper_gap.md) — WP vs implementation gaps
