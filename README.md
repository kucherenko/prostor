# prostor

Open-source live-production studio: a browser-based multi-guest streaming studio
(think StreamYard) on an infinite canvas (think Miro). The producer arranges 16:9
scenes on a board, connects them with transitions, invites guests by link, and
streams the composed program to YouTube, Twitch, TikTok and Telegram at once.
Everything that can run in the browser runs there; a small Rust server does only
what a browser cannot (signaling, RTMP egress, recording).

**Status: pre-alpha, Phase 0 skeleton.** Nothing streams yet. Read
[docs/vision.md](docs/vision.md) first.

## Quick path

1. Install the toolchain: Rust (stable, via rustup), Node 24+, pnpm 10, `just`,
   `wasm-pack`, Google Chrome. On macOS: `brew install just wasm-pack`.
2. `just dev` builds the wasm package, generates the TypeScript types and runs the
   Rust server (`http://localhost:8080`) next to the Vite dev server
   (`http://localhost:5173`, or the next free port; Vite prints the URL).
3. Open the Vite URL. The page shows a scene built by the Rust core running as
   WebAssembly: the hello-world round trip works. `/ws/*` and `/healthz` are
   proxied to the Rust server.

Container alternative: `PROSTOR_SECRET=change-me docker compose up` builds the
single `prostor` image and serves the studio on `http://localhost:8080`. The
secret is required by the compose file; a bare `prostor` binary listens on
`127.0.0.1:8080` unless `PROSTOR_LISTEN` says otherwise.

## Details

| Topic                | Decision                                                                                                   |
| -------------------- | ---------------------------------------------------------------------------------------------------------- |
| Stack                | Rust workspace (sans-IO core + axum server), SvelteKit with Svelte 5 runes and TypeScript, wasm-bindgen    |
| Browser/server split | Capture, compositing, encoding and FLV muxing in the browser; signaling, RTMP, recording on the server     |
| Shared core          | `core/*` compiles to native and `wasm32`; CI runs the protocol tests in both runtimes                       |
| Types                | TypeScript types are generated from `prostor-model` with ts-rs into `web/src/lib/generated` (committed)     |
| Configuration        | `PROSTOR_LISTEN`, `PROSTOR_WEB_DIR`, `PROSTOR_RECORDINGS_DIR`, `PROSTOR_SECRET`, `PROSTOR_LOG`               |
| Decisions            | [docs/adr/](docs/adr/) — start with 0001 (sans-IO core) and 0002 (browser-first boundary)                   |

### Repository layout

```
prostor/
├── Cargo.toml               # workspace: core/*, server/*, spikes/*
├── core/
│   ├── prostor-model/       # domain types, versioned schema, serde, TS export
│   ├── prostor-engine/      # scene state machine, layout, transitions, mixer logic
│   ├── prostor-protocol/    # FLV muxer + RTMP client (sans-IO)
│   └── prostor-wasm/        # wasm-bindgen facade for the browser
├── server/prostor-server/   # axum binary `prostor`: static files, signaling, ingest, egress, recording
├── web/                     # SvelteKit app: /studio (producer), /join/[room] (guest)
├── spikes/                  # throwaway feasibility code; results live in docs/spikes/
├── docs/                    # vision, adr/, acceptance/, spikes/
├── Dockerfile, docker-compose.yml, justfile
└── .github/workflows/ci.yml
```

### Everyday commands

| Command          | What it does                                                        |
| ---------------- | ------------------------------------------------------------------- |
| `just dev`       | Server + Vite with reload                                           |
| `just test`      | `cargo test`, wasm tests in headless Chrome, vitest                 |
| `just lint`      | rustfmt, clippy (`-D warnings`), prettier, eslint, svelte-check     |
| `just build`     | Production build: wasm → types → web → `target/release/prostor`     |
| `just smoke`     | Boots the release server and checks the page in headless Chrome     |
| `just docker`    | Builds the `prostor` image                                          |

`just test-wasm` needs a chromedriver matching your Chrome. It uses `$CHROMEDRIVER`,
then one on `PATH`, then `~/.cache/prostor-browsers` (install with
`npx @puppeteer/browsers install chromedriver@stable --path ~/.cache/prostor-browsers`);
otherwise wasm-pack downloads one.

## Checklist for contributors

- [ ] `just lint` and `just test` pass before you push.
- [ ] Core crates stay sans-IO: no tokio, sockets, files or `web-sys` outside `prostor-wasm`.
- [ ] Model changes regenerate the TypeScript types (`just gen-types`) and are committed.
- [ ] Expensive-to-reverse decisions get an ADR in `docs/adr/`.
- [ ] Conventional commits, English everywhere.

## Next step

Read [docs/vision.md](docs/vision.md), then the ADRs. The Phase 0 plan and the
acceptance script live in `docs/plan-phase0.md` and `docs/acceptance/phase0.md`.

License: [Apache-2.0](LICENSE).
