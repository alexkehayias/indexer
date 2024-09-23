# Build stage
FROM rust:bookworm AS builder

WORKDIR /

COPY ./src ./src
COPY ./Cargo.toml ./Cargo.toml
RUN cargo build --release

# Run stage
FROM debian:bookworm-slim AS runner

# Use the compiled binary rather than cargo
COPY --from=builder /target/release/indexer /indexer

# Initialize index
RUN mkdir -p .index

# Use the compiled binary rather than cargo
COPY --from=builder /target/release/indexer /indexer

# Copy over static files for the web UI, currently these are built and
# checked into the repo but that might change later
COPY ./web-ui/src/index.html ./web-ui/src/index.html
COPY ./web-ui/src/index.js ./web-ui/src/index.js
COPY ./web-ui/src/output.css ./web-ui/src/output.css

# Copy over notes and index it
COPY /Users/alex/Org/notes ./notes
RUN ./indexer  --path ./notes

EXPOSE 2222

ENTRYPOINT ["./indexer", "--serve", "--host", "0.0.0.0", "--port", "2222"]