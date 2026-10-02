# syntax=docker/dockerfile:1

FROM rust:1-slim AS builder
WORKDIR /app

ENV CARGO_BUILD_JOBS=2 \
    CARGO_PROFILE_DIST_LTO=false

COPY . .

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --profile dist --locked -p kosync_rs \
    && cp /app/target/dist/kosync_rs /usr/local/bin/kosync_rs

FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && apt-get clean \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/local/bin/kosync_rs /usr/local/bin/kosync_rs

WORKDIR /app
VOLUME ["/app/data"]
EXPOSE 8090

CMD ["kosync_rs"]
