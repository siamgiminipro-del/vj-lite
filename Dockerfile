FROM rust:1-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY crates ./crates
RUN cargo build --release -p vj-server

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates g++ python3 nodejs && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/vj-server /usr/local/bin/vj-server
COPY ui ./ui
ENV PORT=3000 HOST=0.0.0.0 UI_DIR=/app/ui DB_PATH=/app/data/vj.db
EXPOSE 3000
CMD ["vj-server"]
