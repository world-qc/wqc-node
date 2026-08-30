# Contributing to wqc-node

Thank you for your interest in contributing to **wqc-node**, the libp2p swarm agent that connects local `wqc-core` workers to the World Quantum Computer orchestrator.

## Code of Conduct

This project follows the [Contributor Covenant Code of Conduct](CODE_OF_CONDUCT.md). By participating, you agree to uphold it. Report unacceptable behavior using the contact details in that document.

## How to Contribute

Contributions are welcome in many forms:

- Bug reports and feature requests via [GitHub Issues](https://github.com/world-qc/wqc-node/issues)
- Documentation improvements (including `docs/`)
- Code changes via pull requests

If you plan a larger change, please open an issue first so we can discuss the approach and avoid duplicate work.

## Development Setup

### Prerequisites

- [Rust](https://www.rust-lang.org/tools/install) **1.95** or newer
- A running `wqc-core` instance (HTTP or Unix socket) for end-to-end execution tests
- Orchestrator bootstrap URL, or the [wqc-docs E2E compose stack](https://github.com/world-qc/wqc-docs/blob/main/examples/E2E.md) for multi-node P2P integration testing

### Clone and build

```bash
git clone https://github.com/world-qc/wqc-node.git
cd wqc-node
cargo build
```

### Run locally

```bash
export WQC_CORE_URL="http://127.0.0.1:3000"
export WQC_NODE_PRIVATE_KEY="<base64-32-byte-seed>"
export WQC_BOOTSTRAP_URLS="http://127.0.0.1:9000/api/v1/p2p/bootstrap"
cargo run
```

See [README.md](README.md) and [`docs/OPERATIONS.md`](docs/OPERATIONS.md) for environment variables, admin HTTP endpoints, and troubleshooting.

## Making Changes

1. Fork the repository and create a branch from `main`.
2. Make your changes in a focused, reviewable scope.
3. Run the checks below before opening a pull request.
4. Open a pull request against `main` with a clear description of the change and why it is needed.

### Branch naming

Use short, descriptive names, for example:

- `fix/result-outbox-retry`
- `docs/operations-env-vars`
- `feat/dispatch-signature-check`

## Coding Guidelines

- Write all source code, documentation, and comments in **English**.
- Keep P2P protocol payloads aligned with the orchestrator wire format.
- Follow common Rust conventions (`cargo fmt`, idiomatic error handling).
- Do not commit private keys or production bootstrap credentials.

## Checks

Before submitting a pull request, run:

```bash
cargo fmt --all
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo build --release --locked
```

If you add new behavior, include tests where practical.

## Pull Request Guidelines

A good pull request:

- Has a concise title and description
- Explains the problem and the chosen solution
- Links related issues (for example, `Fixes #123`)
- Passes local checks listed above
- Keeps unrelated changes out of the diff
- Notes any P2P wire-format or admin API changes clearly

Maintainers may request changes or suggest an alternative approach. Once approved, your contribution will be merged.

## Licensing

By contributing, you agree that your contributions will be licensed under the same terms as the project: the [GNU General Public License v3.0](LICENSE).

## Questions

If something is unclear, open a [GitHub Issue](https://github.com/world-qc/wqc-node/issues) or ask in your pull request. We are happy to help you get started.
