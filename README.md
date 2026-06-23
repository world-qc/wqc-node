# wqc-node (The Swarm Agent)

[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](https://www.gnu.org/licenses/gpl-3.0)
[![Status: Alpha](https://img.shields.io/badge/Status-Alpha-yellow.svg)]()

**Become the Computer.** `wqc-node` connects your local `wqc-core` engine to the World Quantum Computer (WQC) swarm over **libp2p**. It participates in the permissionless bid lottery, executes slice sub-tasks, and returns zk-STARK proofs via P2P streams.

Operational details (env vars, Docker, troubleshooting) live in [`docs/OPERATIONS.md`](docs/OPERATIONS.md).

## Why Run a Node?

- **Democratize Quantum Access**: Contribute consumer hardware to a distributed simulation network.
- **Proof of Useful Work**: Compute real quantum circuits and STARK proofs—not meaningless hashes.
- **Fair Rewards**: Participation-based distribution (settlement is handled by the orchestrator today; on-chain claims are Phase 3).

## Core Responsibilities

- **Swarm participation**: Subscribe to task announcements, submit signed bids, receive dispatches.
- **Slice execution**: Forward pruned circuits to `wqc-core`, collect `complex_result` + STARK `proof`.
- **Result delivery**: Stream results back to the orchestrator on `/wqc/tensor-result/1.0.0`.
- **Crash recovery**: Persist pending tasks in SQLite and resume after restart.
- **Admin surface**: Expose `GET /status` and `GET /health` for local monitoring.

## Architecture

```
Orchestrator (libp2p :4001)
    │  Gossip: TaskAnnouncement
    │  Stream: /wqc/tensor-net/1.0.0      ← signed Bid
    │  Stream: /wqc/tensor-dispatch/1.0.0 → SubTask
    │  Stream: /wqc/tensor-result/1.0.0   ← Result + Proof
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
| `/wqc/tensor-result/1.0.0` | Node → Orchestrator | `complex_result` + `proof` + `work_report` |

Wire formats match the [orchestrator README](../wqc-orchestrator/README.md#p2p-protocols-node-facing).

## Quick Start

### Prerequisites

- **Rust** 1.95+ (to build from source)
- **`wqc-core`** running and reachable (`WQC_CORE_URL`)
- **Orchestrator** libp2p bootstrap multiaddr and Ed25519 public key

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
export WQC_ORCHESTRATOR_BOOTSTRAP="/ip4/127.0.0.1/tcp/4001/p2p/12D3KooW..."
export WQC_ORCHESTRATOR_PUBLIC_KEY="<orchestrator-ed25519-pubkey-base64>"
export WQC_NODE_STAKE_WQC="0.05"
export WQC_MAX_QUBITS="26"

cargo run --release
```

### Docker (devnet)

See `world-qc-docker/wqc/compose.yml` for a five-node layout. Typical node env:

```yaml
WQC_NODE_PRIVATE_KEY: <unique per node>
WQC_NODE_STAKE_WQC: "0.05"
WQC_CORE_URL: unix:/var/run/wqc-core-01.sock   # or http://wqc-core-02:3000
WQC_ORCHESTRATOR_BOOTSTRAP: /ip4/10.20.3.11/tcp/4001/p2p/<orchestrator-peer-id>
WQC_ORCHESTRATOR_PUBLIC_KEY: <orchestrator-pubkey-base64>
WQC_DATABASE_URL: sqlite:wqc-node-01.db
WQC_MAX_QUBITS: "26"
WQC_P2P_LISTEN_PORT: "4002"
```

Build and run:

```bash
docker compose -f world-qc-docker/wqc/compose.yml up wqc-node-01
```

## Environment Variables

| Variable | Required | Default | Description |
| :--- | :---: | :--- | :--- |
| `WQC_NODE_PRIVATE_KEY` | yes | — | Base64 Ed25519 seed (32 bytes). Derives libp2p PeerID and bid signatures. |
| `WQC_ORCHESTRATOR_BOOTSTRAP` | yes | — | Comma-separated libp2p multiaddrs. Must include `/p2p/<peer-id>`. |
| `WQC_ORCHESTRATOR_PUBLIC_KEY` | yes | — | Base64 Ed25519 public key of the trusted orchestrator (task ownership in SQLite). |
| `WQC_CORE_URL` | no | `http://localhost:3000` | `wqc-core` base URL or `unix:/path/to.sock`. |
| `WQC_NODE_STAKE_WQC` | no | `0.05` | Human WQC amount sent as `stake_amount` (Planck integer on wire). |
| `WQC_MAX_QUBITS` | no | `30` | Max qubits per sub-task; also advertised as `max_qubit_capability` in bids. |
| `WQC_COMPUTE_TIMEOUT_SECS` | no | `300` | Timeout for `POST /compute` to core. |
| `WQC_P2P_LISTEN_PORT` | no | `4002` | TCP/QUIC listen port for libp2p. |
| `WQC_HTTP_PORT` | no | `8080` | Admin API bind port. |
| `WQC_DATABASE_URL` | no | `sqlite:wqc-node.db` | SQLite path (`sqlite:` prefix optional). |

## Admin HTTP API

| Endpoint | Description |
| :--- | :--- |
| `GET /health` | `{"status":"UP"}` |
| `GET /status` | Pending task count, `max_qubits`, core sysinfo, supported gates |

Task ingress and results use **P2P only**—there is no `/submit` or webhook endpoint on the node.

## Security Model

1. **Bootstrap trust**: The node dials `WQC_ORCHESTRATOR_BOOTSTRAP` and only accepts announce/dispatch streams from that orchestrator's PeerID.
2. **Bid authenticity**: Each bid is signed with `WQC_NODE_PRIVATE_KEY` and includes a lottery proof for `bid_difficulty`.
3. **Capability gating**: Gates from `wqc-core` `GET /gates` map to `supported_features`; the node skips announcements it cannot execute.
4. **Trapdoor audits**: The orchestrator may inject golden sub-tasks; failures can lead to a ban on the orchestrator side.

## Development Roadmap

### Phase 1 — P2P Worker (current)

- [x] libp2p bid / dispatch / result streams
- [x] SQLite pending-task recovery
- [x] `WorkReport` for orchestrator Gas accounting
- [x] `WQC_NODE_STAKE_WQC` → Planck stake on bids
- [ ] P2P result outbox + retry on delivery failure

### Phase 2 — Operations

- [ ] Prometheus metrics (`/status` is available today)
- [ ] Hardware tuning via `wqc-core` (CPU/GPU is a core concern)

### Phase 3 — Sovereign Network

- [ ] libp2p DHT orchestrator discovery (replacing static bootstrap)
- [ ] On-chain $WQC settlement from verified root proofs

## License

Distributed under the GNU General Public License v3.0 (GPLv3). See `LICENSE` for more information.
