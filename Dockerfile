# Build stage
FROM rust:bookworm AS builder

WORKDIR /

## Cache rust dependencies
## https://stackoverflow.com/questions/58473606/cache-rust-dependencies-with-docker-build
RUN mkdir ./src && echo 'fn main() { println!("Dummy!"); }' > ./src/main.rs
COPY ./Cargo.toml .
RUN cargo build --release

## Actually build the app
RUN rm -rf ./src
COPY ./src ./src
RUN touch -a -m ./src/main.rs
RUN cargo build --release

# Run stage
FROM debian:bookworm-slim AS runner

RUN apt update
RUN apt install -y git

# Use the compiled binary rather than cargo
COPY --from=builder /target/release/indexer /indexer

# Copy over static files for the web UI, currently these are built and
# checked into the repo but that might change later
COPY ./web-ui/src/index.html ./web-ui/src/index.html
COPY ./web-ui/src/index.js ./web-ui/src/index.js
COPY ./web-ui/src/output.css ./web-ui/src/output.css
COPY ./web-ui/src/manifest.json ./web-ui/src/manifest.json
COPY ./web-ui/src/service-worker.js ./web-ui/src/service-worker.js

EXPOSE 2222

# Initialize the index and run the server
ENTRYPOINT ["./indexer", "--init", "serve", "--host", "0.0.0.0", "--port", "2222"]
