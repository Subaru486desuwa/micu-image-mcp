FROM rust:1.88-slim-bookworm
RUN apt-get update && apt-get install -y --no-install-recommends git ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && ! command -v python && ! command -v python3
WORKDIR /workspace
COPY . .
RUN git init -q && git add . && ! command -v python && ! command -v python3
RUN cargo build --locked --workspace \
    && cargo test --locked --workspace --all-targets --all-features \
    && cargo build --locked --release --all-features \
    && cargo xtask source-audit \
    && cargo xtask smoke --binary target/release/micu-image-mcp \
    && cargo xtask release-check --tag v0.4.1
