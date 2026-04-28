# wqc-node (The Swarm Agent)

[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](https://www.gnu.org/licenses/gpl-3.0)
[![Status: Alpha](https://img.shields.io/badge/Status-Alpha-yellow.svg)]()

**Become the Computer.** `wqc-node` is the gateway software that connects your local hardware to the World Quantum Computer (WQC) network. It acts as a secure autonomous agent, managing quantum computation tasks, verifying signatures, and proving computational effort via Proof of Useful Work (PoUW).

## Core Responsibilities
- **Task Orchestration**: Receives quantum circuits from authorized orchestrators.
- **Internal State Management**: Persistent task tracking using SQLite to survive crashes/restarts.
- **PoUW Governance**: Autonomously calculates task difficulty and enforces node-specific execution policies.
- **Secure Communication**: Dual-way Ed25519 signature verification for both requests and results.

## Why Run a Node?
- **Democratize Quantum Access**: Help break the monopoly of centralized tech giants.
- **Proof of Useful Work**: Your electricity isn't wasted on meaningless hashes; it powers scientific breakthroughs.
- **Fair Rewards**: Participation-based distribution with zero pre-sale or VC allocation.

## Development Roadmap & Status

The `wqc-node` is evolving alongside the `wqc-core` engine. We are currently in the transition from a single-tenant executor to a robust, multi-tenant swarm participant.

### ✅ Phase 1: Persistence & Security (Current)
*Focus: Creating a robust, trustless execution environment.*
- [x] **Multi-Tenant Isolation**: Unique task management per Orchestrator using composite keys (Pubkey + TaskID).
- [x] **SQLite Persistence**: Automatic recovery of pending tasks after node restarts.
- [x] **Signature Enforcement**: Mandatory Ed25519 verification for all incoming `/submit` requests.
- [x] **Autonomous Difficulty Calculation**: Node-side verification of "Zero-Bit" difficulty to prevent orchestrator cheating.
- [x] **Execution Metrics**: Accurate reporting of `wall_time`, `iterations`, and `difficulty` in webhooks.

### 🚧 Phase 2: Resource Optimization (Upcoming)
*Focus: Intelligent task scheduling and hardware efficiency.*
- [ ] **Dynamic Difficulty Scaling**: Adjusting accepted difficulty ranges based on real-time CPU/GPU load.
- [ ] **Advanced Hardware Abstraction**: Seamless switching between CPU (AVX-512) and GPU (CUDA/Metal) via `wqc-core`.
- [ ] **Health Monitoring**: Prometheus/Grafana integration for node performance tracking.

### 🚀 Phase 3: Autonomous Economic Agent
*Focus: Decentralized discovery and incentive automation.*
- [ ] **P2P Discovery**: Finding orchestrators via DHT instead of static config.
- [ ] **On-chain Settlement**: Automated $WQC reward claims based on submitted PoUW proofs.
- [ ] **SLA Enforcement**: Automatic blacklisting of orchestrators with high webhook failure rates.

## Usage (CLI Alpha)

### Environment Configuration

| Variable | Description | Default Value |
| :--- | :--- | :--- |
| `WQC_NODE_PRIVATE_KEY` | Ed25519 private key (Base64) for node identity and signing webhooks. | (Required) |
| `WQC_ALLOWED_ORCHESTRATOR_PUBKEYS` | Comma-separated list of trusted Orchestrator public keys. | (Empty: Reject all) |
| `WQC_MIN_DIFFICULTY` | Minimum acceptable PoUW difficulty (number of leading zero bits). | `10` |
| `WQC_MAX_DIFFICULTY` | Maximum acceptable PoUW difficulty (number of leading zero bits). | `24` |
| `WQC_MAX_QUBITS` | Maximum number of qubits allowed for a single task. | `30` |
| `WQC_MAX_MEMORY_COST_KB` | Maximum allowed memory hardness parameter for Argon2id. | `2097152` (2GB) |
| `WQC_DEV_MODE` | Enable development mode to skip signature verification (`true`/`false`). | `false` |
| `WQC_DATABASE_URL` | File path for the SQLite persistence database. | `sqlite:wqc-node.db` |

---

### Technical Note on `memory_cost_kb`
In the WQC protocol, `memory_cost_kb` is not just a resource limit, but a **cryptographic difficulty parameter**. It defines the Argon2id memory-hardness used to bind the quantum state vector to the PoUW hash. Increasing this value makes the computation "heavier" in terms of RAM bandwidth, providing stronger ASIC resistance and proof of actual hardware resource allocation.

### Running the Node
```bash
./wqc-node start --wallet <YOUR_WQC_ADDRESS> --intensity high
```

## Security & Protocol

`wqc-node` strictly follows the WQC Trust Protocol. Every `/submit` request must be signed. The node converts the incoming `ComputeRequest` into an internal `ComputeTask`, injecting a locally-calculated `difficulty` (number of leading zero bits) before passing it to the engine.

### Verification Logic
The node calculates the difficulty based on the circuit complexity:
`Difficulty (Bits) = Base(10) + (Qubits / 4) + (Gates / 50)`

The resulting webhook payload includes:
- `difficulty`: The target zero-bit count enforced by the node.
- `iterations`: The actual number of hash attempts performed by `wqc-core`.
- `execution_time_ms`: Pure computational time (wall-clock).

> **Note**: Orchestrators monitor the ratio between `difficulty` and `iterations`. Statistical anomalies (e.g., finding a 20-bit proof in 100 iterations) may lead to task rejection.

## Community
Join the swarm on [Discord]() or [X]().

## License
Distributed under the GNU General Public License v3.0 (GPLv3). See `LICENSE` for more information.
