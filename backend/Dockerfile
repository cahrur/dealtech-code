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
    ca-certificates curl git \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/backend .
COPY --from=builder /app/migrations ./migrations
COPY entrypoint.sh .
RUN chmod +x entrypoint.sh
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=10s --retries=3 \
    CMD curl -f http://localhost:8080/health || exit 1
ENTRYPOINT ["./entrypoint.sh"]
