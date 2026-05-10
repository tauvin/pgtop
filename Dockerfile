# Multi-stage build: compile in a full Rust image, run on a slim base.
FROM rust:1.90-bookworm AS builder

WORKDIR /app
COPY . .
RUN cargo build --release --locked

FROM debian:bookworm-slim

# ca-certificates is needed for TLS to managed Postgres providers
# (RDS, Cloud SQL, …) which present certificates from public CAs.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/pgtop /usr/local/bin/pgtop

ENTRYPOINT ["pgtop"]
