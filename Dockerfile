# Build stage
FROM rust:1-alpine AS builder

RUN apk add --no-cache musl-dev

WORKDIR /build

# Cache dependencies by copying manifests first
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && \
    cargo build --release && \
    rm -rf src

# Build the real application
COPY src/ src/
RUN touch src/main.rs && cargo build --release

# Runtime stage
FROM alpine:3.24

RUN adduser -D -h /home/torrenttui torrenttui

COPY --from=builder /build/target/release/torrenttui /usr/local/bin/torrenttui

# BitTorrent listen ports (matches listen_port_range: 6881..6891)
EXPOSE 6881-6890

# Download directory
VOLUME /downloads
# Config *and* session state: torrents persist across container restarts only
# because the session lives under the config directory (`.../torrenttui/session`).
# It used to sit in librqbit's own shared data directory, outside this volume,
# so this mount silently persisted nothing but config.
VOLUME /home/torrenttui/.config/torrenttui

RUN mkdir -p /downloads && chown torrenttui:torrenttui /downloads

USER torrenttui

ENTRYPOINT ["torrenttui"]
CMD ["-d", "/downloads"]
