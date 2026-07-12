FROM rust:1.88-bookworm AS builder

WORKDIR /src
COPY . .
RUN cargo build --release -p edger-orchestrator --bin edger

FROM denoland/deno:debian

LABEL org.opencontainers.image.source="https://github.com/djalmajr/edger" \
      org.opencontainers.image.licenses="O'Saasy-1.0"

USER root
WORKDIR /app

COPY --from=builder /src/target/release/edger /usr/local/bin/edger
COPY workers/cpanel /app/workers/cpanel

RUN groupadd --system --gid 10001 edger \
    && useradd --system --uid 10001 --gid 10001 --home-dir /app --shell /usr/sbin/nologin edger \
    && chown -R edger:edger /app

ENV DENO_DIR=/tmp/deno \
    EDGER_JS_RUNTIME=process \
    HOME=/tmp \
    PORT=3000 \
    RUNTIME_WORKER_DIRS=/app/workers

EXPOSE 3000
USER 10001:10001
ENTRYPOINT ["edger"]
