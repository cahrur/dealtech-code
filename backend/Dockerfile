# ── Stage 1: dependency cache ──────────────────────────────────────────────────
FROM rust:1.88-slim-bookworm AS deps
WORKDIR /app
RUN apt-get update && apt-get install -y \
    pkg-config libssl-dev git \
    && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock* ./
RUN mkdir -p src && echo 'fn main(){}' > src/main.rs
RUN cargo build --release
RUN rm -rf src

# ── Stage 2: build ─────────────────────────────────────────────────────────────
FROM deps AS builder
COPY . .
RUN touch src/main.rs && cargo build --release

# ── Stage 3: runtime ───────────────────────────────────────────────────────────
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y \
    ca-certificates curl git unzip \
    && rm -rf /var/lib/apt/lists/*

# Install Nuclei
RUN curl -sL https://github.com/projectdiscovery/nuclei/releases/download/v3.3.7/nuclei_3.3.7_linux_amd64.zip -o /tmp/nuclei.zip \
    && unzip -q /tmp/nuclei.zip -d /tmp \
    && mv /tmp/nuclei /usr/local/bin/nuclei \
    && chmod +x /usr/local/bin/nuclei \
    && rm -rf /tmp/nuclei.zip \
    && nuclei -update-templates -silent 2>/dev/null || true
# NOTE: runs as root — required for docker.sock access (container management)
# For production hardening: use tecnativa/docker-socket-proxy to restrict docker API
WORKDIR /app
COPY --from=builder /app/target/release/backend .
COPY --from=builder /app/migrations ./migrations
COPY entrypoint.sh .
RUN chmod +x entrypoint.sh
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=10s --retries=3 \
    CMD curl -f http://localhost:8080/health || exit 1
ENTRYPOINT ["./entrypoint.sh"]
