# Build stage
FROM rust:bookworm AS builder

WORKDIR /
COPY ./src ./src
COPY ./Cargo.toml ./Cargo.toml
COPY ./Cargo.lock ./Cargo.lock
RUN cargo build --release

# Run stage
FROM debian:bookworm-slim AS runner

WORKDIR /
RUN mkdir -p .index

EXPOSE 2222

COPY --from=builder /target/release/indexer /indexer
ENTRYPOINT ["./indexer", "--serve", "--host", "0.0.0.0", "--port", "2222"]
