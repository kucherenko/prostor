# syntax=docker/dockerfile:1.7
# Builds the single `prostor` image: the Rust server binary plus the built web app.

ARG RUST_VERSION=1.93
ARG NODE_VERSION=24
# Must support `--profile` (0.14+); the rustwasm.github.io installer ships an older build.
ARG WASM_PACK_VERSION=0.15.0

# ---- 1. Rust core compiled to WebAssembly ------------------------------------
FROM rust:${RUST_VERSION}-bookworm AS wasm
ARG WASM_PACK_VERSION
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    rustup target add wasm32-unknown-unknown \
    && cargo install wasm-pack --locked --version "${WASM_PACK_VERSION}"
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
COPY core core
COPY server server
COPY spikes spikes
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    wasm-pack build core/prostor-wasm --target web --profile wasm-release \
        --out-dir /out/pkg --out-name prostor_wasm

# ---- 2. Web app --------------------------------------------------------------
FROM node:${NODE_VERSION}-bookworm-slim AS web
RUN npm install -g pnpm@10
WORKDIR /web
COPY web/package.json web/pnpm-lock.yaml web/.npmrc ./
RUN --mount=type=cache,target=/root/.local/share/pnpm/store \
    pnpm install --frozen-lockfile
COPY web .
COPY --from=wasm /out/pkg src/lib/wasm/pkg
RUN pnpm build

# ---- 3. Server binary --------------------------------------------------------
FROM rust:${RUST_VERSION}-bookworm AS server
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
COPY core core
COPY server server
COPY spikes spikes
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release -p prostor-server \
    && mkdir -p /out && cp target/release/prostor /out/prostor

# ---- 4. Runtime --------------------------------------------------------------
FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home --home-dir /app prostor \
    && mkdir -p /app/web /recordings \
    && chown prostor:prostor /recordings
COPY --from=server /out/prostor /usr/local/bin/prostor
COPY --from=web /web/build /app/web
ENV PROSTOR_LISTEN=0.0.0.0:8080 \
    PROSTOR_WEB_DIR=/app/web \
    PROSTOR_RECORDINGS_DIR=/recordings \
    PROSTOR_LOG=info
EXPOSE 8080
VOLUME ["/recordings"]
USER prostor
WORKDIR /app
ENTRYPOINT ["/usr/local/bin/prostor"]
