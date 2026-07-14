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
COPY workers/core/cpanel /opt/edger/core-workers/cpanel
COPY workers/core/webide/manifest.yaml /opt/edger/core-workers/webide/manifest.yaml
COPY workers/core/webide/dist /opt/edger/core-workers/webide/dist

RUN groupadd --system --gid 10001 edger \
    && useradd --system --uid 10001 --gid 10001 --home-dir /app --shell /usr/sbin/nologin edger \
    && mkdir -p /app/workers /app/core-worker-overlays \
    && chown -R edger:edger /app \
    && chmod -R a-w /opt/edger/core-workers

ENV DENO_DIR=/tmp/deno \
    EDGER_JS_RUNTIME=process \
    EDGER_CORE_WORKER_DIR=/opt/edger/core-workers \
    EDGER_CORE_WORKER_OVERLAY_DIR=/app/core-worker-overlays \
    HOME=/tmp \
    PORT=3000 \
    RUNTIME_WORKER_DIRS=/app/workers

EXPOSE 3000
VOLUME ["/app/workers", "/app/core-worker-overlays"]
USER 10001:10001
ENTRYPOINT ["edger"]
