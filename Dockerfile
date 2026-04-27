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
FROM alpine:3.23

RUN adduser -D -h /home/torrenttui torrenttui

COPY --from=builder /build/target/release/torrenttui /usr/local/bin/torrenttui

# BitTorrent listen ports (matches listen_port_range: 6881..6891)
EXPOSE 6881-6890

# Download directory
VOLUME /downloads
# Config and session persistence
VOLUME /home/torrenttui/.config/torrenttui

RUN mkdir -p /downloads && chown torrenttui:torrenttui /downloads

USER torrenttui

ENTRYPOINT ["torrenttui"]
CMD ["-d", "/downloads"]
