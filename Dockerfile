# syntax=docker/dockerfile:1
# Release image for wqc-node (GKE Spot miners / local).
#
# Build (push-ar.sh uses context = wqc-node/):
#   docker build --platform linux/amd64 -f wqc-node/Dockerfile \
#     -t world-qc/wqc-node:latest wqc-node
#
# On Apple Silicon, the builder runs on BUILDPLATFORM (native) and
# cross-compiles to amd64. Full linux/amd64 under QEMU SIGSEGVs in collect2/cc
# (same pattern as wqc-composer / wqc-p2p-proxy).

FROM --platform=$BUILDPLATFORM rust:1.95-slim-bookworm AS builder

ARG TARGETARCH
ARG BUILDARCH

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    pkg-config \
    $(if [ "$TARGETARCH" = "amd64" ] && [ "$BUILDARCH" != "amd64" ]; then \
        echo build-essential gcc-x86-64-linux-gnu g++-x86-64-linux-gnu libc6-dev-amd64-cross; \
      else \
        echo build-essential libssl-dev; \
      fi) \
    && rm -rf /var/lib/apt/lists/* /var/cache/apt/archives/*

WORKDIR /app

# Step 1: Pre-compile dependencies only
COPY Cargo.toml Cargo.lock* ./
RUN mkdir src && echo "fn main() {}" > src/main.rs

RUN set -eux; \
    if [ "$TARGETARCH" = "amd64" ] && [ "$BUILDARCH" != "amd64" ]; then \
      rustup target add x86_64-unknown-linux-gnu; \
      export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc; \
      export CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc; \
      export CXX_x86_64_unknown_linux_gnu=x86_64-linux-gnu-g++; \
      export AR_x86_64_unknown_linux_gnu=x86_64-linux-gnu-ar; \
      export CARGO_TERM_PROGRESS_WHEN=never; \
      cargo build --release --target x86_64-unknown-linux-gnu; \
      rm -f target/x86_64-unknown-linux-gnu/release/deps/wqc_node*; \
    else \
      cargo build --release; \
      rm -f target/release/deps/wqc_node*; \
    fi

# Step 2: Build actual source code
COPY src ./src

RUN set -eux; \
    if [ "$TARGETARCH" = "amd64" ] && [ "$BUILDARCH" != "amd64" ]; then \
      export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc; \
      export CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc; \
      export CXX_x86_64_unknown_linux_gnu=x86_64-linux-gnu-g++; \
      export AR_x86_64_unknown_linux_gnu=x86_64-linux-gnu-ar; \
      export CARGO_TERM_PROGRESS_WHEN=never; \
      cargo build --release --target x86_64-unknown-linux-gnu; \
      cp target/x86_64-unknown-linux-gnu/release/wqc-node /usr/local/bin/wqc-node; \
    else \
      cargo build --release; \
      cp target/release/wqc-node /usr/local/bin/wqc-node; \
    fi; \
    cp Cargo.lock /Cargo.lock

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /usr/local/bin/wqc-node /usr/local/bin/
COPY --from=builder /Cargo.lock /

ENV RUST_LOG=info

CMD ["wqc-node"]
