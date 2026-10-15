# prostor task runner. `just` lists the recipes.

set shell := ["bash", "-euo", "pipefail", "-c"]

root := justfile_directory()
web := root / "web"
wasm_pkg := web / "src/lib/wasm/pkg"
gen_dir := web / "src/lib/generated"

# chromedriver for `test-wasm`: $CHROMEDRIVER, then PATH, then the @puppeteer/browsers
# cache (`npx @puppeteer/browsers install chromedriver@stable --path ~/.cache/prostor-browsers`).
# Empty means "let wasm-pack download one".
chromedriver := env("CHROMEDRIVER", `command -v chromedriver 2>/dev/null || { for f in "$HOME"/.cache/prostor-browsers/chromedriver/*/chromedriver-*/chromedriver; do [ -x "$f" ] && echo "$f" && break; done; } || true`)

# List recipes.
default:
    @just --list --unsorted

# Run the server (:8080) and the Vite dev server (:5173, proxies /ws and /healthz) with reload.
dev: build-wasm gen-types
    #!/usr/bin/env bash
    set -euo pipefail
    trap 'kill 0' EXIT INT TERM
    (cd "{{ web }}" && pnpm dev) &
    if command -v cargo-watch >/dev/null 2>&1; then
        cargo watch -q -w core -w server -x 'run -p prostor-server'
    else
        cargo run -p prostor-server
    fi

# Build the wasm package into web/src/lib/wasm/pkg (size-optimized profile).
build-wasm:
    wasm-pack build core/prostor-wasm --target web --profile wasm-release \
        --out-dir "{{ wasm_pkg }}" --out-name prostor_wasm

# Regenerate the TypeScript types from prostor-model (ts-rs, see .cargo/config.toml).
gen-types:
    rm -rf "{{ gen_dir }}"
    cargo test -p prostor-model --lib --quiet

# Run every test suite: native, wasm in headless Chrome, web.
test: test-rust test-wasm test-web

# Native Rust tests.
test-rust:
    cargo test --workspace

# Rust tests compiled to wasm, run in headless Chrome.
test-wasm:
    #!/usr/bin/env bash
    set -euo pipefail
    args=(--headless --chrome)
    if [ -n "{{ chromedriver }}" ]; then args+=(--chromedriver "{{ chromedriver }}"); fi
    wasm-pack test "${args[@]}" core/prostor-protocol
    wasm-pack test "${args[@]}" core/prostor-wasm

# Web unit and component tests (vitest).
test-web:
    cd "{{ web }}" && pnpm test

# Formatting, clippy, eslint, prettier and svelte-check.
lint:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    cd "{{ web }}" && pnpm lint && pnpm check

# Format Rust and web sources in place.
fmt:
    cargo fmt --all
    cd "{{ web }}" && pnpm format

# Full production build: wasm → types → web → server (target/release/prostor).
build: build-wasm gen-types
    cd "{{ web }}" && pnpm build
    cargo build --release -p prostor-server

# Boot the release server with the built web app and drive it with headless Chrome.
smoke: build
    #!/usr/bin/env bash
    set -euo pipefail
    port="${PROSTOR_SMOKE_PORT:-18080}"
    PROSTOR_LISTEN="127.0.0.1:${port}" PROSTOR_WEB_DIR="{{ web }}/build" \
        "{{ root }}/target/release/prostor" &
    server=$!
    trap 'kill "$server" 2>/dev/null || true' EXIT
    for _ in $(seq 1 50); do
        curl -fs "http://127.0.0.1:${port}/healthz" >/dev/null 2>&1 && break
        sleep 0.2
    done
    cd "{{ web }}" && PROSTOR_URL="http://127.0.0.1:${port}/" pnpm smoke

# Build, boot the release server with a temporary recordings directory and a secret, stream
# 10 s from Chrome's fake camera through /studio (to the recording and, when ffmpeg is
# installed, to an RTMP listener), and check both files with ffprobe.
e2e: build
    #!/usr/bin/env bash
    set -euo pipefail
    port="${PROSTOR_E2E_PORT:-18081}"
    rtmp_port="${PROSTOR_E2E_RTMP_PORT:-19350}"
    recordings="$(mktemp -d)"
    secret="e2e-$(od -An -N8 -tx1 /dev/urandom | tr -d ' \n')"
    PROSTOR_LISTEN="127.0.0.1:${port}" PROSTOR_WEB_DIR="{{ web }}/build" \
        PROSTOR_RECORDINGS_DIR="$recordings" PROSTOR_SECRET="$secret" \
        PROSTOR_ALLOW_PRIVATE_DESTINATIONS=1 \
        "{{ root }}/target/release/prostor" &
    server=$!
    trap 'kill "$server" $listener 2>/dev/null || true' EXIT
    listener=""
    rtmp_env=()
    if command -v ffmpeg >/dev/null 2>&1; then
        # An RTMP destination: ffmpeg listens once and writes what it receives.
        ffmpeg -v error -y -listen 1 -i "rtmp://127.0.0.1:${rtmp_port}/live/e2e" \
            -c copy -f flv "$recordings/rtmp-received.flv" &
        listener=$!
        rtmp_env=(PROSTOR_E2E_RTMP_URL="rtmp://127.0.0.1:${rtmp_port}/live" PROSTOR_E2E_RTMP_KEY=e2e \
            PROSTOR_E2E_RTMP_OUT="$recordings/rtmp-received.flv")
        sleep 0.5
    fi
    for _ in $(seq 1 50); do
        curl -fs "http://127.0.0.1:${port}/healthz" >/dev/null 2>&1 && break
        sleep 0.2
    done
    cd "{{ web }}" && env PROSTOR_URL="http://127.0.0.1:${port}/" PROSTOR_RECORDINGS_DIR="$recordings" \
        PROSTOR_SECRET="$secret" "${rtmp_env[@]}" pnpm e2e

# Build the `prostor` Docker image.
docker:
    docker build -t prostor .

# Remove build outputs.
clean:
    cargo clean
    rm -rf "{{ web }}/build" "{{ web }}/.svelte-kit" "{{ wasm_pkg }}"
