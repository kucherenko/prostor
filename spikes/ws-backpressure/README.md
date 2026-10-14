# Spike (d): WebSocket ingest backpressure

Throwaway code behind `docs/spikes/0004-ws-ingest-backpressure.md`. It measures what happens
when a browser pushes 6 Mbit/s of binary frames over a WebSocket to an axum server that
cannot keep up.

## Quick path

```sh
cargo build -p ws-backpressure --release          # the sink (port 9471)
cd spikes/ws-backpressure/web && pnpm install
node run.mjs                                      # all scenarios, results in ../results/
node run.mjs --scenario S2,S3a --repeat 1         # a subset
```

`run.mjs` builds and starts the server itself, drives the page in headless Chrome
(`puppeteer-core` with the installed Google Chrome), samples CPU and writes one JSON per
scenario plus `summary.json`.

## Manual run

```sh
target/release/ws-backpressure --port 9471
open 'http://127.0.0.1:9471/?rate_bps=4000000&policy=drop-delta'
```

Query parameters: `bitrate` (bit/s), `fps`, `gop` (s), `keyRatio`, `duration` (s),
`policy` (`none` | `drop-delta` | `drop-all`), `threshold` (bytes), `mode` (`ws` |
`wsstream`), `batch` (frames per message), `rate_bps` and `slow_ms` (server-side consumer
limits). `POST /limit?id=<id>&rate_bps=0` changes a live connection's limit;
`GET /stats/<id>` shows the server's view.
