FROM rust:1-bookworm AS builder
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends libsqlite3-dev pkg-config && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
COPY crates/cito-native ./crates/cito-native
COPY plugins ./plugins
RUN cargo build --locked --release --bin cito-native --bin cito-plugin-telegram

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libsqlite3-0 && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home cito \
    && mkdir -p /data /app/plugins/telegram/bin && chown -R cito:cito /data /app
WORKDIR /app
COPY --from=builder /app/target/release/cito-native /usr/local/bin/cito
COPY plugins/telegram/plugin.json /app/plugins/telegram/plugin.json
COPY --from=builder /app/target/release/cito-plugin-telegram /app/plugins/telegram/bin/cito-plugin-telegram
ENV CITO_PLUGIN_DIR=/app/plugins CITO_HUB_DATA_DIR=/data CITO_HUB_BIND=0.0.0.0 CITO_HUB_PORT=4291
USER cito
EXPOSE 4291
ENTRYPOINT ["cito", "hub"]
