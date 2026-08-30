# wqc-node (The Swarm Agent)

[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](https://www.gnu.org/licenses/gpl-3.0)
[![Status: Beta](https://img.shields.io/badge/Status-Beta-orange.svg)]()
[![CI](https://github.com/world-qc/wqc-node/actions/workflows/ci.yml/badge.svg)](https://github.com/world-qc/wqc-node/actions/workflows/ci.yml)

**Become the Computer.** `wqc-node` connects your local `wqc-core` engine to the World Quantum Computer (WQC) swarm over **libp2p**. It participates in the permissionless bid lottery, executes slice sub-tasks, and returns zk-STARK proofs via P2P streams.

Operational details (env vars, troubleshooting) live in [`docs/OPERATIONS.md`](docs/OPERATIONS.md).

## Role in the WQC pipeline

```
client → wqc-orchestrator → wqc-node (this repo) → wqc-core
```

On public testnet, most operators run **[`wqc-miner`](https://github.com/world-qc/wqc-miner)**, which starts `wqc-node` and `wqc-core` together and injects env from `settings.toml`. Direct `cargo run` of this repo is for development, headless servers, and integration testing.

Task lifecycle (announce → bid → dispatch → compute → result) is normative in [wqc-docs `spec/architecture-current.md` §3](https://github.com/world-qc/wqc-docs/blob/main/spec/architecture-current.md#3-task-lifecycle).

## Why Run a Node?

- **Democratize Quantum Access**: Contribute consumer hardware to a distributed simulation network.
- **Proof of Useful Work**: Compute real quantum circuits and STARK proofs—not meaningless hashes.
- **Fair Rewards**: Participation-based distribution (settlement is orchestrator-side today; on-chain claims are planned).

## Core Responsibilities

- **Swarm participation**: Subscribe to task announcements, submit signed bids, receive dispatches.
- **Slice execution**: Forward pruned circuits to `wqc-core`, collect `complex_result` or `sample_result` + STARK `proof`.
- **Result delivery**: Stream results back to the orchestrator on `/wqc/tensor-result/1.0.0`.
- **Leaf PCS**: When nominated via `/wqc/tensor-pcs-req/1.0.0`, call core `POST /leaf_pcs` and stream `/wqc/tensor-pcs/1.0.0`. Memory-gate **refuse** (422) is permanent; successful builds are cached in the PCS outbox for delivery retry.
- **PCS open call**: After majority nomination is exhausted, bid on `/wqc/tensor-pcs-bid/1.0.0` only if the connected core reports `pcs_memory_policy=spill` via `GET /sysinfo` (set `WQC_PCS_MEMORY_POLICY=spill` on **wqc-core**, not on the node). Nominated builders fetch the leaf proof from CAS, verify SHA-256, then prove and stream the bundle.
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

## P2P (with orchestrator)

| Stage | Protocol |
| :--- | :--- |
| Announce | Gossip `wqc-global-announcements` or stream `/wqc/task-announce/1.0.0` |
| Bid | `/wqc/tensor-net/1.0.0` |
| Dispatch | `/wqc/tensor-dispatch/1.0.0` |
| Result | `/wqc/tensor-result/1.0.0` |

Leaf PCS streams (`/wqc/tensor-pcs-req/1.0.0`, `/wqc/tensor-pcs/1.0.0`, open-call announce/bid) are documented in the same spec. Framing, signature payloads, and message shapes are normative in [wqc-docs `spec/p2p-protocols.md`](https://github.com/world-qc/wqc-docs/blob/main/spec/p2p-protocols.md).

## Quick Start

### Testnet (recommended)

Use **[`wqc-miner`](https://github.com/world-qc/wqc-miner)** — paste your operator node key from [testnet.world-qc.io](https://testnet.world-qc.io), save settings, and start mining. The launcher sets `WQC_NODE_PRIVATE_KEY`, `WQC_TESTNET_NODE_KEY`, `WQC_BOOTSTRAP_URLS`, `WQC_CORE_URL`, and `WQC_MAX_MEMORY_GB` for you.

### Manual run (development)

#### Prerequisites

- **Rust** 1.95+ (to build from source)
- **`wqc-core`** running and reachable (`WQC_CORE_URL`)
- **Orchestrator** libp2p bootstrap (discovered via HTTP at node startup)

#### Generate a node key

The node identity is a 32-byte Ed25519 seed (Base64). The libp2p PeerID is derived from this key at startup.

```bash
# Example: generate a random 32-byte seed and encode
openssl rand -base64 32
```

Set `WQC_NODE_PRIVATE_KEY` to that value. On public testnet also set `WQC_TESTNET_NODE_KEY` from the dashboard.

#### Minimal local run

```bash
export WQC_NODE_PRIVATE_KEY="<base64-32-byte-seed>"
export WQC_CORE_URL="http://localhost:3000"
export WQC_BOOTSTRAP_URLS="http://localhost:9000/api/v1/p2p/bootstrap"
export WQC_NODE_STAKE_WQC="0.05"
export WQC_MAX_MEMORY_GB="1"

cargo run --release
```

### Multi-node E2E (developers)

For a five-node reference stack (orchestrator, Redis, object store, cores, nodes), see [wqc-docs `examples/E2E.md`](https://github.com/world-qc/wqc-docs/blob/main/examples/E2E.md) and [`examples/compose.yml`](https://github.com/world-qc/wqc-docs/blob/main/examples/compose.yml). Requires a monorepo checkout with sibling repos (`wqc-core`, `wqc-orchestrator`, etc.).

Typical node env in that layout:

```yaml
WQC_NODE_PRIVATE_KEY: <unique per node>
WQC_NODE_STAKE_WQC: "0.05"
WQC_CORE_URL: unix:/var/run/wqc-core-01.sock   # or http://wqc-core-02:3000
WQC_BOOTSTRAP_URLS: http://wqc-orchestrator-01:9000/api/v1/p2p/bootstrap
WQC_DATABASE_URL: sqlite:wqc-node-01.db
WQC_MAX_MEMORY_GB: "1"
WQC_P2P_LISTEN_PORT: "4002"
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
| `GET /metrics` | Prometheus exposition format |

`GET /status` reads `system_memory_used_kb`, `system_memory_total_kb`, and `cpu_usage_percent` from the connected `wqc-core` `GET /sysinfo`. **If core is unreachable the node still answers `200` with those three fields zeroed** instead of failing, so a monitor cannot tell a dead core from a genuine reading. The fetch failure is only written to the node log; no metric covers it (`wqc_node_core_requests_total` counts `POST /compute` outcomes, not this call). Poll core's own `/health` if you need to distinguish the two.

Task ingress and results use **P2P only**—there is no `/submit` or webhook endpoint on the node.

## Security Model

1. **Bootstrap trust**: At startup the node `GET`s `WQC_BOOTSTRAP_URLS`, then dials the returned multiaddrs and pins that orchestrator PeerID.
2. **Announcement authenticity**: Each `TaskAnnouncement` carries an orchestrator Ed25519 signature verified against the bootstrap `public_key_b64`.
3. **Dispatch authenticity**: Each `SubTask` carries an orchestrator Ed25519 signature verified against the bootstrap `public_key_b64`.
4. **Bid authenticity**: Each bid is signed with `WQC_NODE_PRIVATE_KEY` and includes a lottery proof for `bid_difficulty`.
5. **Capability gating**: Gates from `wqc-core` `GET /gates` map to `supported_features`; the node skips announcements it cannot execute.
6. **Trapdoor audits**: The orchestrator may inject golden sub-tasks; failures can lead to a ban on the orchestrator side.

## Upcoming

- libp2p DHT orchestrator discovery (replacing static bootstrap).
- On-chain economic layer (orchestrator / L2).

## Documentation

- [`docs/OPERATIONS.md`](docs/OPERATIONS.md) — operator runbook (env vars, lifecycle, troubleshooting)
- [wqc-docs `spec/p2p-protocols.md`](https://github.com/world-qc/wqc-docs/blob/main/spec/p2p-protocols.md) — wire formats and protocol IDs
- [wqc-docs `spec/architecture-current.md`](https://github.com/world-qc/wqc-docs/blob/main/spec/architecture-current.md) — swarm topology and off-chain economy
- [`wqc-miner`](https://github.com/world-qc/wqc-miner) — recommended testnet launcher
- [`wqc-core`](https://github.com/world-qc/wqc-core) — compute engine this process calls

## Requirements

- **Rust**: 1.95+ (see `AGENTS.md`)
- **`wqc-core`**: reachable at `WQC_CORE_URL` (started by [`wqc-miner`](https://github.com/world-qc/wqc-miner) on testnet, or manually for dev)
- **RAM**: `WQC_MAX_MEMORY_GB` drives advertised qubit capability; [`wqc-miner`](https://github.com/world-qc/wqc-miner) sets this from `max_memory_gb` in [`settings.toml`](https://github.com/world-qc/wqc-miner/blob/main/settings.toml.example). Advanced: `export WQC_MPS_MAX_BOND_DIM=…` before launch affects core accuracy ceiling (see [`wqc-core` `doc/tn-engine.md`](https://github.com/world-qc/wqc-core/blob/main/doc/tn-engine.md))

## Contributing

Contributions are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup, coding guidelines, and the pull request process.

## License

Distributed under the GNU General Public License v3.0 (GPLv3). See `LICENSE` for more information.
