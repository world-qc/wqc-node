# wqc-node (The Swarm Agent)

[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](https://www.gnu.org/licenses/gpl-3.0)
[![Status: Alpha](https://img.shields.io/badge/Status-Alpha-yellow.svg)]()

**Become the Computer.** `wqc-node` is the client software that connects your device to the World Quantum Computer network. By running this node, you provide computational power for quantum simulations and receive $WQC rewards in return.

## Why Run a Node?
- **Democratize Quantum Access**: Help break the monopoly of centralized tech giants.
- **Proof of Useful Work**: Your electricity isn't wasted on meaningless hashes; it powers scientific breakthroughs.
- **Fair Rewards**: Participation-based distribution with zero pre-sale or VC allocation.

## Supported Hardware
- **NVIDIA GPUs**: RTX 30/40/50 series (8GB+ VRAM recommended).
- **Apple Silicon**: M-series (M1-M4) via Unified Memory.
- **Linux/Windows/macOS**: Bare-metal performance via Rust.

## Usage (CLI Alpha)
```bash
export WQC_NODE_PRIVATE_KEY="<base64-encoded-32-byte-ed25519-seed>"
export WQC_ALLOWED_ORCHESTRATOR_PUBKEYS="<orchestrator_pubkey_1_b64>,<orchestrator_pubkey_2_b64>"
# Optional only for local testing:
# export WQC_DEV_MODE=true
./wqc-node start --wallet <YOUR_WQC_ADDRESS> --intensity high
```

`WQC_NODE_PRIVATE_KEY` is required. The node signs webhook payloads using Ed25519 and sends:
- `X-WQC-Node-PublicKey`
- `X-WQC-Timestamp`
- `X-WQC-Nonce`
- `X-WQC-Signature`

`/submit` now requires request signature verification by default. The node validates:
- `X-WQC-Orchestrator-PublicKey`
- `X-WQC-Timestamp`
- `X-WQC-Nonce`
- `X-WQC-Signature`

Signature message format for `/submit` is:
`WQC-REQUEST-V1\n{timestamp}\n{nonce}\n{sha256(body_hex)}`

## Community
Join the swarm on [Discord]() or [X]().

## License
Distributed under the GNU General Public License v3.0 (GPLv3). See `LICENSE` for more information.
