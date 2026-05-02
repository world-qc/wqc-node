# wqc-node (The Swarm Agent)

[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](https://www.gnu.org/licenses/gpl-3.0)
[![Status: Alpha](https://img.shields.io/badge/Status-Alpha-yellow.svg)]()

**Become the Computer.** `wqc-node` is the gateway software that connects your local hardware to the World Quantum Computer (WQC) network. It acts as a secure autonomous agent, managing quantum computation tasks, verifying signatures, and proving computational effort via Proof of Useful Work (PoUW).

## Why Run a Node?
- **Democratize Quantum Access**: Help break the monopoly of centralized tech giants.
- **Proof of Useful Work**: Your electricity isn't wasted on meaningless hashes; it powers scientific breakthroughs.
- **Fair Rewards**: Participation-based distribution with zero pre-sale or VC allocation.

## Core Responsibilities
- **Autonomous Registration**: Self-registers to orchestrators and establishes trust dynamically.
- **Task Orchestration**: Receives quantum circuits and executes them using the `wqc-core` engine.
- **Internal State Management**: Persistent task tracking using SQLite to survive crashes/restarts.
- **PoUW Governance**: Autonomously calculates task difficulty and enforces node-specific execution policies.
- **Secure Communication**: Dual-way Ed25519 signature verification using a **TOFU (Trust on First Use)** security model.

## Development Roadmap & Status

The `wqc-node` is evolving alongside the `wqc-core` engine. We are currently in the transition from a single-tenant executor to a robust, multi-tenant swarm participant.

### ✅ Phase 1: Persistence & Security (Current)
*Focus: Creating a robust, trustless execution environment.*
- [x] **Zero-Config Trust (TOFU)**: Dynamic orchestrator public key discovery via registration handshake.
- [x] **Persistence**: Automatic recovery of pending tasks from SQLite after node restarts.
- [x] **Autonomous Difficulty**: Node-side bit-level difficulty enforcement to prevent protocol abuse.
- [x] **Audit-Ready Reporting**: Full integration of `iterations` count and `wall_time` in result webhooks.
- [x] **Signature Enforcement**: Mandatory Ed25519 verification for all `/submit` and `/register` flows.

### 🚧 Phase 2: Resource Optimization (Upcoming)
*Focus: Intelligent task scheduling and hardware efficiency.*
- [ ] **Dynamic Difficulty Scaling**: Adjusting accepted difficulty ranges based on real-time hardware load.
- [ ] **Advanced Hardware Abstraction**: Seamless switching between CPU (AVX-512) and GPU (CUDA/Metal).
- [ ] **Health Monitoring**: Prometheus/Grafana integration for performance tracking.

### 🚀 Phase 3: Sovereign Network
*Focus: Decentralized discovery and incentive automation.*
- [ ] **P2P Discovery**: Finding orchestrators via libp2p/DHT instead of static URLs.
- [ ] **On-chain Settlement**: Automated $WQC reward claims based on PoUW proofs.

## Usage (CLI Alpha)

### Environment Configuration

| Variable | Description | Default Value |
| :--- | :--- | :--- |
| `WQC_NODE_ADVERTISED_URL` | The public URL of this node (e.g., `http://your-ip:8080`). | (Computed/Local) |
| `WQC_NODE_PRIVATE_KEY` | Ed25519 private key (Base64) for node identity and signing. | (Required) |
| `WQC_CORE_URL` | The URL of Core (e.g., `http://localhost:3000`). | (Required) |
| `WQC_ORCHESTRATOR_URLS` | Comma-separated list of Orchestrator URLs to join. | (Required) |
| `WQC_MIN_DIFFICULTY` | Minimum acceptable PoUW difficulty (zero bits). | `10` |
| `WQC_MAX_DIFFICULTY` | Maximum acceptable PoUW difficulty (zero bits). | `24` |
| `WQC_MAX_QUBITS` | Maximum number of qubits allowed for a single task. | `30` |
| `WQC_MAX_MEMORY_COST_KB`| Maximum memory hardness parameter for Argon2id. | `2097152` (2GB) |
| `WQC_DATABASE_URL` | File path for the SQLite persistence database. | `sqlite:wqc-node.db` |

---

### Technical Note on `memory_cost_kb`
In the WQC protocol, `memory_cost_kb` is not just a resource limit, but a **cryptographic difficulty parameter**. It defines the Argon2id memory-hardness used to bind the quantum state vector to the PoUW hash. Increasing this value makes the computation "heavier" in terms of RAM bandwidth, providing stronger ASIC resistance and proof of actual hardware resource allocation.

### Running the Node
```bash
export WQC_NODE_ADVERTISED_URL="http://your-ip:8080"
./wqc-node start --wallet <YOUR_WQC_ADDRESS> --intensity high
```

## Security & Protocol

### The Handshake

`wqc-node` uses a **Trust on First Use (TOFU)** model to simplify setup while maintaining high security:

1. **Discovery**: Upon startup, the node sends a signed registration request to the configured `WQC_ORCHESTRATOR_URLS`.
2. **Key Exchange**: The node captures the Orchestrator's public key from the `X-WQC-Orchestrator-PublicKey` response header.
3. **Verification**: Subsequent `/submit` requests from that Orchestrator are strictly verified against this learned key.
4. **Audit**: Orchestrators immediately issue a `audit-` task to verify the node's computational integrity.

### Verification Logic

`wqc-node` strictly follows the WQC Trust Protocol. Every `/submit` request must be signed. The node converts the incoming `ComputeRequest` into an internal `ComputeTask`, injecting a locally-calculated `difficulty` (number of leading zero bits) before passing it to the engine.

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
