# wqc-node (The Swarm Agent)

[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](https://www.gnu.org/licenses/gpl-3.0)
[![Status: Alpha](https://img.shields.io/badge/Status-Alpha-yellow.svg)]()
[![CI](https://github.com/world-qc/wqc-node/actions/workflows/ci.yml/badge.svg)](https://github.com/world-qc/wqc-node/actions/workflows/ci.yml)

**Become the Computer.** `wqc-node` connects your local `wqc-core` engine to the World Quantum Computer (WQC) swarm over **libp2p**. It participates in the permissionless bid lottery, executes slice sub-tasks, and returns zk-STARK proofs via P2P streams.

Operational details (env vars, Docker, troubleshooting) live in [`docs/OPERATIONS.md`](docs/OPERATIONS.md).

## Why Run a Node?

- **Democratize Quantum Access**: Contribute consumer hardware to a distributed simulation network.
- **Proof of Useful Work**: Compute real quantum circuits and STARK proofs—not meaningless hashes.
- **Fair Rewards**: Participation-based distribution (settlement is handled by the orchestrator today; on-chain claims are Phase 3).

## Core Responsibilities

- **Swarm participation**: Subscribe to task announcements, submit signed bids, receive dispatches.
- **Slice execution**: Forward pruned circuits to `wqc-core`, collect `complex_result` or `sample_result` + STARK `proof`.
- **Result delivery**: Stream results back to the orchestrator on `/wqc/tensor-result/1.0.0`.
- **Leaf PCS (quorum candidate)**: When the orchestrator names this node via `/wqc/tensor-pcs-req/1.0.0`, calls core `POST /leaf_pcs` and streams `/wqc/tensor-pcs/1.0.0`. Success earns `R_pcs`. Memory-gate **refuse** (HTTP 422) reports `refused: true` permanently (no retry); the orchestrator may request another quorum majority node before open call or compose fallback. Non-nominated nodes' proofs are pruned after `WQC_PCS_UNREQUESTED_TTL_SECS`. An in-flight guard prevents duplicate concurrent proves; a successful build is cached in the PCS outbox so delivery retries only re-send. When core is unreachable, a health-gate backs off prove retries (cached PCS delivery still proceeds).
- **PCS open call (spill builder)**: When majority PCS nomination is exhausted, the orchestrator publishes a CAS-backed open call on `/wqc/tensor-pcs-open/1.0.0`. Only nodes whose connected `wqc-core` reports `pcs_memory_policy=spill` via `GET /sysinfo` bid on `/wqc/tensor-pcs-bid/1.0.0`. If nominated (`request_kind=open_call`), the node fetches the leaf proof from CAS, verifies SHA-256, calls core `POST /leaf_pcs`, and streams the bundle. Open-call builders need not retain the proof locally.
- **Crash recovery**: Persist pending tasks in SQLite and resume after restart.
- **Admin surface**: Expose `GET /status` and `GET /health` for local monitoring.

## Architecture

```
Orchestrator (libp2p :4001)
    │  Gossip: TaskAnnouncement
    │  Stream: /wqc/tensor-net/1.0.0      ← signed Bid
    │  Stream: /wqc/tensor-dispatch/1.0.0 → SubTask
    │  Stream: /wqc/tensor-result/1.0.0   ← Result + Proof
    │  Stream: /wqc/tensor-pcs-req/1.0.0  → LeafPcs request (majority or open-call builder)
    │  Stream: /wqc/tensor-pcs/1.0.0      ← LeafPcsBundle (nominated node only)
    │  Stream: /wqc/tensor-pcs-open/1.0.0 → CAS open-call announcement
    │  Stream: /wqc/tensor-pcs-bid/1.0.0  ← spill-policy open-call bid
    ▼
wqc-node (libp2p :4002, HTTP admin :8080)
    │  POST /compute
    ▼
wqc-core (HTTP or unix socket)
```

The node runs **one sub-task at a time** per process. The orchestrator tracks in-flight work per node and will not dispatch the next slice for the same parent task until the previous result is ingested.

## P2P Protocols (with Orchestrator)

| Protocol ID | Direction | Purpose |
| :--- | :--- | :--- |
| `wqc-global-announcements` (gossip) | Orchestrator → Nodes | `TaskAnnouncement` after client submit |
| `/wqc/task-announce/1.0.0` | Orchestrator → Node | Alternate announce stream (same payload) |
| `/wqc/tensor-net/1.0.0` | Node → Orchestrator | Signed lottery `Bid` |
| `/wqc/tensor-dispatch/1.0.0` | Orchestrator → Node | `SubTask` for execution |
| `/wqc/tensor-result/1.0.0` | Node → Orchestrator | `result_type` + `complex_result` + optional `sample_result` + `proof` + `work_report` |
| `/wqc/tensor-pcs-req/1.0.0` | Orchestrator → Node | Signed PCS build request (majority nominee or open-call builder) |
| `/wqc/tensor-pcs/1.0.0` | Node → Orchestrator | `sub_task_id` + `leaf_pcs_b64`, or `refused: true` (nominated node only) |
| `/wqc/tensor-pcs-open/1.0.0` | Orchestrator → Node | Signed CAS-backed PCS open-call announcement |
| `/wqc/tensor-pcs-bid/1.0.0` | Node → Orchestrator | Spill-policy bid (`pcs_memory_policy=spill`) |

### PCS open-call eligibility

Open-call bids are gated by the **connected core**, not node-local env:

1. On each open-call announcement, the node probes `wqc-core` `GET /sysinfo`.
2. Bid only when `pcs_memory_policy == "spill"`.
3. Set `WQC_PCS_MEMORY_POLICY=spill` on **wqc-core** (not on wqc-node).

Remote-core deployments (`WQC_CORE_URL` pointing at another host) therefore bid according to that core's policy, avoiding env drift where a node bids but its core refuses `/leaf_pcs`.

Wire formats match the [orchestrator README](../wqc-orchestrator/README.md#p2p-protocols-node-facing).

### `sample_counts` (P2P)

Signed `SubTask` may include `output_mode`, `shots`, `classical_bit_count`, and orchestrator-generated `sample_seed`. The node forwards these to `wqc-core` and returns `result_type` + `sample_result` on `/wqc/tensor-result/1.0.0`.
`counts` bitstrings follow **Qiskit order** (rightmost = `cbit 0`).

## Quick Start

### Prerequisites

- **Rust** 1.95+ (to build from source)
- **`wqc-core`** running and reachable (`WQC_CORE_URL`)
- **Orchestrator** libp2p bootstrap (discovered via HTTP at node startup)

### Generate a node key

The node identity is a 32-byte Ed25519 seed (Base64). The libp2p PeerID is derived from this key at startup.

```bash
# Example: generate a random 32-byte seed and encode
openssl rand -base64 32
```

Set `WQC_NODE_PRIVATE_KEY` to that value. Log the derived PeerID from startup output when registering the node with the orchestrator dev faucet (if enabled).

### Minimal local run

```bash
export WQC_NODE_PRIVATE_KEY="<base64-32-byte-seed>"
export WQC_CORE_URL="http://localhost:3000"
export WQC_BOOTSTRAP_URLS="http://localhost:9000/api/v1/p2p/bootstrap"
export WQC_NODE_STAKE_WQC="0.05"
export WQC_MAX_MEMORY_GB="1"

cargo run --release
```

### Docker (devnet)

See `world-qc-docker/devnet/compose.yml` for a five-node layout. Typical node env:

```yaml
WQC_NODE_PRIVATE_KEY: <unique per node>
WQC_NODE_STAKE_WQC: "0.05"
WQC_CORE_URL: unix:/var/run/wqc-core-01.sock   # or http://wqc-core-02:3000
WQC_BOOTSTRAP_URLS: http://wqc-orchestrator-01:9000/api/v1/p2p/bootstrap
WQC_DATABASE_URL: sqlite:wqc-node-01.db
WQC_MAX_MEMORY_GB: "1"
WQC_P2P_LISTEN_PORT: "4002"
```

Build and run:

```bash
docker compose -f world-qc-docker/devnet/compose.yml up wqc-node-01
```

## Environment Variables

| Variable | Required | Default | Description |
| :--- | :---: | :--- | :--- |
| `WQC_NODE_PRIVATE_KEY` | yes | — | Base64 Ed25519 seed (32 bytes). Derives libp2p PeerID and bid signatures. |
| `WQC_BOOTSTRAP_URLS` | yes | — | Comma-separated full bootstrap HTTP(S) URLs (e.g. `http://host:9000/api/v1/p2p/bootstrap`). Failover left-to-right. |
| `WQC_CORE_URL` | no | `http://localhost:3000` | `wqc-core` base URL or `unix:/path/to.sock`. |
| `WQC_NODE_STAKE_WQC` | no | `0.05` | Human WQC amount sent as `stake_amount` (Planck integer on wire). |
| `WQC_TESTNET_NODE_KEY` | testnet | — | Node Key from [testnet.world-qc.io](https://testnet.world-qc.io). Derives `operator_id` and signs `operator_sig` on bids. Required on public testnet. |
| `WQC_MAX_MEMORY_GB` | no | `16` | WQC memory budget (GiB). Capped at host RAM minus 1/2 GiB reserve (`memory_budget.rs`). Derives `max_qubit_capability` as `floor(log2(budget / 16))` (dense `2^n × 16` envelope). |
| `WQC_COMPUTE_TIMEOUT_SECS` | no | `300` | Timeout for `POST /compute` to core. |
| `WQC_CORE_HEALTH_FAIL_THRESHOLD` | no | `3` | Consecutive unreachable core errors before opening the health-gate. |
| `WQC_CORE_HEALTH_BACKOFF_SECS` | no | `30` | Seconds to skip core `/compute` and `/leaf_pcs` prove calls while the gate is open. |
| `WQC_P2P_LISTEN_PORT` | no | `4002` | TCP/QUIC listen port for libp2p. |
| `WQC_HTTP_PORT` | no | `8080` | Admin API bind port. |
| `WQC_DATABASE_URL` | no | `sqlite:wqc-node.db` | SQLite path (`sqlite:` prefix optional). |
| `WQC_RESULT_RETRY_INTERVAL_SECS` | no | `5` | Background interval for retrying undelivered P2P results. |
| `WQC_PCS_RETRY_INTERVAL_SECS` | no | `30` | Background interval for retrying requested-but-undelivered leaf PCS bundles. |
| `WQC_PCS_UNREQUESTED_TTL_SECS` | no | `21600` | Drop a retained proof after this long with no PCS request (this node lost the proof-winner draw). |
| `WQC_PCS_TIMEOUT_SECS` | no | `7200` | Wall-clock budget for core `POST /leaf_pcs` (open-call builds included). |
| `WQC_TASK_RETENTION_SECS` | no | `86400` | Delete `completed`/`failed` SQLite `tasks` older than this many seconds. `0` disables. |
| `WQC_TASK_PRUNE_INTERVAL_SECS` | no | `3600` | Background interval for terminal-task prune. |

## Admin HTTP API

| Endpoint | Description |
| :--- | :--- |
| `GET /health` | `{"status":"UP"}` |
| `GET /status` | Pending compute tasks, **outbox** pending results, `max_qubits`, `max_memory_gib`, core sysinfo, supported gates |

Task ingress and results use **P2P only**—there is no `/submit` or webhook endpoint on the node.

## Security Model

1. **Bootstrap trust**: At startup the node `GET`s `WQC_BOOTSTRAP_URLS`, then dials the returned multiaddrs and pins that orchestrator PeerID.
2. **Announcement authenticity**: Each `TaskAnnouncement` carries an orchestrator Ed25519 signature verified against the bootstrap `public_key_b64`.
3. **Dispatch authenticity**: Each `SubTask` carries an orchestrator Ed25519 signature verified against the bootstrap `public_key_b64`.
4. **Bid authenticity**: Each bid is signed with `WQC_NODE_PRIVATE_KEY` and includes a lottery proof for `bid_difficulty`.
5. **Capability gating**: Gates from `wqc-core` `GET /gates` map to `supported_features`; the node skips announcements it cannot execute.
6. **Trapdoor audits**: The orchestrator may inject golden sub-tasks; failures can lead to a ban on the orchestrator side.

## Development Roadmap

### Phase 1 — P2P Worker (current)

- [x] libp2p bid / dispatch / result streams
- [x] SQLite pending-task recovery
- [x] `WorkReport` for orchestrator Gas accounting
- [x] `WQC_NODE_STAKE_WQC` → Planck stake on bids
- [x] P2P result outbox (SQLite `pending_results`) + background retry

### Phase 2 — Operations & execution model

- [x] Forward `output_mode`, `shots`, `classical_bit_count`, `sample_seed`; return `sample_result` on P2P
- [x] Prometheus metrics (`GET /metrics` on the admin HTTP port)
- [ ] Hardware tuning via `wqc-core` (CPU/GPU is a core concern)

### Phase 3 — Sovereign Network

- [ ] libp2p DHT orchestrator discovery (replacing static bootstrap)
- [ ] On-chain $WQC settlement from verified root proofs

## Contributing

Contributions are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup, coding guidelines, and the pull request process.

## License

Distributed under the GNU General Public License v3.0 (GPLv3). See `LICENSE` for more information.
