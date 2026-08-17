FROM rust:1-slim AS builder
WORKDIR /app
COPY Cargo.toml ./
COPY src ./src
RUN cargo build --release

FROM gcc:latest AS shim-builder
WORKDIR /shim
COPY listen_override.c ./
RUN gcc -shared -fPIC -o listen_override.so listen_override.c -ldl

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/slashbench-actix-web ./app
COPY --from=shim-builder /shim/listen_override.so ./listen_override.so
EXPOSE 8080
CMD ["./app"]
