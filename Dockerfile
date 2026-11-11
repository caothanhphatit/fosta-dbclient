# syntax=docker/dockerfile:1.7
#
# Fosta Studio web edition: the UI and `fosta-studio-server` in one small image.
#
#   docker build -t fosta-studio .
#   docker run -d -p 8080:8080 -v fosta-studio-data:/data fosta-studio
#   docker logs <container>     # the sign-in address with the access token
#
# Stages: `ui` builds the TypeScript UI, `server` builds the Rust server (feature `web`, rustls, no OpenSSL),
# the final stage is distroless (glibc, CA certificates, no shell) running as a non-root user.

ARG NODE_VERSION=22.21
ARG RUST_VERSION=1.98.0

FROM node:${NODE_VERSION}-bookworm-slim AS ui
WORKDIR /src
COPY package.json package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY index.html tsconfig.json vite.config.ts ./
COPY src ./src
RUN npm run build

FROM rust:${RUST_VERSION}-slim-bookworm AS server
WORKDIR /src/src-tauri
# The desktop build aborts on panic; the server unwinds so one failing request cannot stop it.
ENV CARGO_PROFILE_RELEASE_PANIC=unwind \
    CARGO_TERM_COLOR=never
# Dependencies first, against stub sources, so this layer stays cached until Cargo.toml/Cargo.lock change.
COPY src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/build.rs ./
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    mkdir -p src/server \
    && echo "" > src/lib.rs \
    && echo "fn main() {}" > src/main.rs \
    && echo "fn main() {}" > src/server/main.rs \
    && cargo build --release --locked --no-default-features --features web --bin fosta-studio-server \
    && rm -rf src
# The assistant embeds the user docs and the changelog (ai/app_help.rs, include_str! from ../docs).
COPY docs ../docs
COPY CHANGELOG.md ../CHANGELOG.md
COPY src-tauri/src ./src
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    find src -name '*.rs' -exec touch {} + \
    && cargo build --release --locked --no-default-features --features web --bin fosta-studio-server \
    && mkdir -p /out/data /out/lib \
    && cp target/release/fosta-studio-server /out/
# Oracle Instant Client (downloaded into /data at first use) needs libaio, which the distroless image does not have.
RUN apt-get update -qq && apt-get install -y -qq --no-install-recommends libaio1 \
    && cp -P /usr/lib/x86_64-linux-gnu/libaio.so.1* /out/lib/

FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f
ARG VERSION=dev
LABEL org.opencontainers.image.title="Fosta Studio" \
      org.opencontainers.image.description="Fosta Studio web edition: a database client for 11 engines in your browser" \
      org.opencontainers.image.source="https://github.com/caothanhphatit/fosta-dbclient" \
      org.opencontainers.image.url="https://github.com/caothanhphatit/fosta-dbclient" \
      org.opencontainers.image.licenses="GPL-3.0-only" \
      org.opencontainers.image.version="${VERSION}"
COPY --from=server /out/fosta-studio-server /app/fosta-studio-server
COPY --from=server /out/lib/ /usr/lib/x86_64-linux-gnu/
COPY --from=ui /src/dist /app/ui
# An empty /data owned by the runtime user, so a new named volume is writable.
COPY --from=server --chown=65532:65532 /out/data /data
ENV FOSTA_STUDIO_DATA=/data \
    FOSTA_STUDIO_UI=/app/ui \
    FOSTA_STUDIO_LISTEN=0.0.0.0:8080
USER 65532:65532
VOLUME ["/data"]
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 CMD ["/app/fosta-studio-server", "healthcheck"]
ENTRYPOINT ["/app/fosta-studio-server"]
CMD ["serve"]
