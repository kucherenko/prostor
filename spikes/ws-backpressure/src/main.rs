//! Spike (d): binary WebSocket ingest sink for backpressure measurement.
//!
//! The browser page under `web/` sends synthetic "video frames" at a target
//! bitrate. This server consumes them at a configurable rate so that TCP flow
//! control pushes back on the browser, and records per-connection statistics
//! (throughput, one-way latency, inter-arrival gaps, dropped sequence numbers).
//!
//! Wire format of a binary message: one or more records, each
//! `u32 LE record length` followed by a 16-byte header
//! (`u8 kind` 1 = key / 2 = delta, `u8` pad, `u16` pad, `u32 LE seq`,
//! `f64 LE client timestamp in ms since the Unix epoch`) and the payload.
//!
//! Results are discussed in `docs/spikes/0004-ws-ingest-backpressure.md`.

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    Json, Router,
    extract::{
        Path, Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tower_http::services::ServeDir;

const RECORD_PREFIX_LEN: usize = 4;
const HEADER_LEN: usize = 16;
const WEB_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/web");

#[derive(Clone, Default)]
struct AppState {
    conns: Arc<Mutex<HashMap<String, Arc<Conn>>>>,
    listener: Arc<Mutex<Option<SockInfo>>>,
}

/// Live connection: limits are atomics so `POST /limit` can change them mid-run.
struct Conn {
    /// Consumption limit in bits per second; 0 means unlimited.
    rate_bps: AtomicU64,
    /// Fixed sleep per message in milliseconds; 0 means none.
    slow_ms: AtomicU64,
    /// Ack period in milliseconds; 0 disables acks.
    ack_ms: u64,
    /// Last consumed sequence number and its client send time (f64 bits), for acks.
    acked_seq: AtomicU64,
    acked_sent_ms: AtomicU64,
    raw: Mutex<Raw>,
}

/// Sent back to the browser every `ack_ms`: what the server has consumed so far.
#[derive(Serialize)]
struct Ack {
    seq: u64,
    sent_ms: f64,
    server_ms: f64,
}

/// Raw per-connection measurements; summarised on demand by `GET /stats`.
#[derive(Default)]
struct Raw {
    started_unix_ms: f64,
    started: Option<Instant>,
    duration_ms: f64,
    closed: bool,
    close_reason: Option<String>,
    messages: u64,
    frames: u64,
    key_frames: u64,
    delta_frames: u64,
    bytes: u64,
    max_frame_bytes: u64,
    max_message_bytes: u64,
    bytes_per_second: Vec<u64>,
    latency_ms: Vec<f32>,
    latency_by_second: Vec<SecondAgg>,
    gap_ms: Vec<f32>,
    last_arrival: Option<Instant>,
    last_seq: Option<u32>,
    seq_missing: u64,
    seq_out_of_order: u64,
}

#[derive(Default, Clone, Serialize)]
struct SecondAgg {
    count: u64,
    mean_ms: f32,
    max_ms: f32,
    #[serde(skip)]
    sum_ms: f64,
}

#[derive(Serialize)]
struct Summary {
    count: usize,
    mean: f32,
    p50: f32,
    p95: f32,
    max: f32,
}

#[derive(Serialize)]
struct Stats {
    id: String,
    rate_bps: u64,
    slow_ms: u64,
    started_unix_ms: f64,
    duration_ms: f64,
    closed: bool,
    close_reason: Option<String>,
    messages: u64,
    frames: u64,
    key_frames: u64,
    delta_frames: u64,
    bytes: u64,
    max_frame_bytes: u64,
    max_message_bytes: u64,
    mean_bits_per_second: f64,
    bytes_per_second: Vec<u64>,
    latency_ms: Summary,
    latency_by_second: Vec<SecondAgg>,
    gap_ms: Summary,
    seq_missing: u64,
    seq_out_of_order: u64,
}

#[derive(Serialize, Clone, Copy)]
struct SockInfo {
    listener_so_rcvbuf: usize,
    listener_so_sndbuf: usize,
}

fn summarize(values: &[f32]) -> Summary {
    if values.is_empty() {
        return Summary {
            count: 0,
            mean: 0.0,
            p50: 0.0,
            p95: 0.0,
            max: 0.0,
        };
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let pick = |q: f64| sorted[((sorted.len() - 1) as f64 * q).round() as usize];
    Summary {
        count: sorted.len(),
        mean: (sorted.iter().map(|v| *v as f64).sum::<f64>() / sorted.len() as f64) as f32,
        p50: pick(0.5),
        p95: pick(0.95),
        max: *sorted.last().expect("non-empty"),
    }
}

fn unix_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_secs_f64()
        * 1000.0
}

impl Conn {
    fn new(rate_bps: u64, slow_ms: u64, ack_ms: u64) -> Self {
        Self {
            rate_bps: AtomicU64::new(rate_bps),
            slow_ms: AtomicU64::new(slow_ms),
            ack_ms,
            acked_seq: AtomicU64::new(0),
            acked_sent_ms: AtomicU64::new(0f64.to_bits()),
            raw: Mutex::new(Raw::default()),
        }
    }

    async fn stats(&self, id: &str) -> Stats {
        let raw = self.raw.lock().await;
        let duration_ms = if raw.closed {
            raw.duration_ms
        } else {
            raw.started
                .map(|s| s.elapsed().as_secs_f64() * 1000.0)
                .unwrap_or(0.0)
        };
        Stats {
            id: id.to_string(),
            rate_bps: self.rate_bps.load(Ordering::Relaxed),
            slow_ms: self.slow_ms.load(Ordering::Relaxed),
            started_unix_ms: raw.started_unix_ms,
            duration_ms,
            closed: raw.closed,
            close_reason: raw.close_reason.clone(),
            messages: raw.messages,
            frames: raw.frames,
            key_frames: raw.key_frames,
            delta_frames: raw.delta_frames,
            bytes: raw.bytes,
            max_frame_bytes: raw.max_frame_bytes,
            max_message_bytes: raw.max_message_bytes,
            mean_bits_per_second: if duration_ms > 0.0 {
                raw.bytes as f64 * 8.0 / (duration_ms / 1000.0)
            } else {
                0.0
            },
            bytes_per_second: raw.bytes_per_second.clone(),
            latency_ms: summarize(&raw.latency_ms),
            latency_by_second: raw.latency_by_second.clone(),
            gap_ms: summarize(&raw.gap_ms),
            seq_missing: raw.seq_missing,
            seq_out_of_order: raw.seq_out_of_order,
        }
    }
}

#[derive(Deserialize)]
struct IngestQuery {
    id: Option<String>,
    #[serde(default)]
    rate_bps: u64,
    #[serde(default)]
    slow_ms: u64,
    #[serde(default)]
    ack_ms: u64,
}

async fn ingest(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(q): Query<IngestQuery>,
) -> Response {
    let id = q.id.unwrap_or_else(|| format!("anon-{}", unix_ms() as u64));
    let conn = Arc::new(Conn::new(q.rate_bps, q.slow_ms, q.ack_ms));
    state.conns.lock().await.insert(id.clone(), conn.clone());
    ws.max_message_size(64 << 20)
        .on_upgrade(move |socket| handle_ingest(socket, id, conn))
}

async fn handle_ingest(socket: WebSocket, id: String, conn: Arc<Conn>) {
    tracing::info!(
        id,
        rate_bps = conn.rate_bps.load(Ordering::Relaxed),
        slow_ms = conn.slow_ms.load(Ordering::Relaxed),
        ack_ms = conn.ack_ms,
        "ingest started"
    );
    let start = Instant::now();
    {
        let mut raw = conn.raw.lock().await;
        raw.started = Some(start);
        raw.started_unix_ms = unix_ms();
    }
    let mut next_allowed = start;
    let mut close_reason = None;

    let (mut sink, mut stream) = socket.split();
    // Acks run on their own task so the consumer's sleeps do not delay them;
    // an ack reports what has been consumed, which is the point.
    let ack_task = (conn.ack_ms > 0).then(|| {
        let conn = conn.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_millis(conn.ack_ms));
            loop {
                ticker.tick().await;
                let ack = Ack {
                    seq: conn.acked_seq.load(Ordering::Relaxed),
                    sent_ms: f64::from_bits(conn.acked_sent_ms.load(Ordering::Relaxed)),
                    server_ms: unix_ms(),
                };
                let text = serde_json::to_string(&ack).expect("ack json");
                if sink.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
        })
    });

    while let Some(message) = stream.next().await {
        let message = match message {
            Ok(m) => m,
            Err(e) => {
                close_reason = Some(e.to_string());
                break;
            }
        };
        let data = match message {
            Message::Binary(data) => data,
            Message::Close(frame) => {
                close_reason = frame.map(|f| format!("close {} {}", f.code, f.reason.as_str()));
                break;
            }
            Message::Text(_) | Message::Ping(_) | Message::Pong(_) => continue,
        };

        let now = Instant::now();
        let now_ms = unix_ms();
        let elapsed_s = now.duration_since(start).as_secs() as usize;
        {
            let mut raw = conn.raw.lock().await;
            raw.messages += 1;
            // Count everything on the wire (record prefixes included) so the
            // page can subtract it from what it handed to `send()`.
            raw.bytes += data.len() as u64;
            raw.max_message_bytes = raw.max_message_bytes.max(data.len() as u64);
            if raw.bytes_per_second.len() <= elapsed_s {
                raw.bytes_per_second.resize(elapsed_s + 1, 0);
                raw.latency_by_second
                    .resize(elapsed_s + 1, SecondAgg::default());
            }
            raw.bytes_per_second[elapsed_s] += data.len() as u64;
            if let Some(last) = raw.last_arrival {
                raw.gap_ms
                    .push(now.duration_since(last).as_secs_f32() * 1000.0);
            }
            raw.last_arrival = Some(now);

            let mut off = 0usize;
            while off + RECORD_PREFIX_LEN <= data.len() {
                let len = u32::from_le_bytes(
                    data[off..off + RECORD_PREFIX_LEN]
                        .try_into()
                        .expect("4 bytes"),
                ) as usize;
                let rec_start = off + RECORD_PREFIX_LEN;
                let rec_end = rec_start + len;
                if len < HEADER_LEN || rec_end > data.len() {
                    tracing::warn!(id, off, len, "malformed record");
                    break;
                }
                let rec = &data[rec_start..rec_end];
                let kind = rec[0];
                let seq = u32::from_le_bytes(rec[4..8].try_into().expect("4 bytes"));
                let sent_ms = f64::from_le_bytes(rec[8..16].try_into().expect("8 bytes"));
                let latency = (now_ms - sent_ms) as f32;

                raw.frames += 1;
                raw.max_frame_bytes = raw.max_frame_bytes.max(len as u64);
                match kind {
                    1 => raw.key_frames += 1,
                    _ => raw.delta_frames += 1,
                }
                raw.latency_ms.push(latency);
                let agg = &mut raw.latency_by_second[elapsed_s];
                agg.count += 1;
                agg.sum_ms += latency as f64;
                agg.max_ms = agg.max_ms.max(latency);
                agg.mean_ms = (agg.sum_ms / agg.count as f64) as f32;
                if let Some(last) = raw.last_seq {
                    if seq > last + 1 {
                        raw.seq_missing += u64::from(seq - last - 1);
                    } else if seq <= last {
                        raw.seq_out_of_order += 1;
                    }
                }
                raw.last_seq = Some(seq);
                conn.acked_seq.store(u64::from(seq), Ordering::Relaxed);
                conn.acked_sent_ms
                    .store(sent_ms.to_bits(), Ordering::Relaxed);
                off = rec_end;
            }
        }

        // Consumer-side limits: sleeping here stops reading the socket, so the
        // kernel buffers fill and TCP flow control reaches the browser.
        let rate = conn.rate_bps.load(Ordering::Relaxed);
        if rate > 0 {
            let cost = Duration::from_secs_f64(data.len() as f64 * 8.0 / rate as f64);
            next_allowed = next_allowed.max(now) + cost;
            tokio::time::sleep_until(next_allowed.into()).await;
        }
        let slow = conn.slow_ms.load(Ordering::Relaxed);
        if slow > 0 {
            tokio::time::sleep(Duration::from_millis(slow)).await;
        }
    }

    if let Some(task) = ack_task {
        task.abort();
    }
    let mut raw = conn.raw.lock().await;
    raw.closed = true;
    raw.duration_ms = start.elapsed().as_secs_f64() * 1000.0;
    raw.close_reason = close_reason;
    tracing::info!(
        id,
        frames = raw.frames,
        bytes = raw.bytes,
        mbit_per_s =
            (raw.bytes as f64 * 8.0 / raw.duration_ms * 1000.0 / 1e6 * 100.0).round() / 100.0,
        latency_p95_ms = summarize(&raw.latency_ms).p95,
        seq_missing = raw.seq_missing,
        reason = raw.close_reason.as_deref().unwrap_or("eof"),
        "ingest closed"
    );
}

async fn stats_all(State(state): State<AppState>) -> Json<Vec<Stats>> {
    let conns = state.conns.lock().await;
    let mut out = Vec::with_capacity(conns.len());
    for (id, conn) in conns.iter() {
        out.push(conn.stats(id).await);
    }
    out.sort_by(|a, b| a.started_unix_ms.total_cmp(&b.started_unix_ms));
    Json(out)
}

async fn stats_one(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let conn = state.conns.lock().await.get(&id).cloned();
    match conn {
        Some(conn) => Json(conn.stats(&id).await).into_response(),
        None => (StatusCode::NOT_FOUND, "unknown connection").into_response(),
    }
}

#[derive(Deserialize)]
struct LimitQuery {
    id: String,
    rate_bps: Option<u64>,
    slow_ms: Option<u64>,
}

async fn set_limit(State(state): State<AppState>, Query(q): Query<LimitQuery>) -> Response {
    let conn = state.conns.lock().await.get(&q.id).cloned();
    let Some(conn) = conn else {
        return (StatusCode::NOT_FOUND, "unknown connection").into_response();
    };
    if let Some(rate) = q.rate_bps {
        conn.rate_bps.store(rate, Ordering::Relaxed);
    }
    if let Some(slow) = q.slow_ms {
        conn.slow_ms.store(slow, Ordering::Relaxed);
    }
    tracing::info!(
        id = q.id,
        rate_bps = conn.rate_bps.load(Ordering::Relaxed),
        slow_ms = conn.slow_ms.load(Ordering::Relaxed),
        "limit changed"
    );
    StatusCode::NO_CONTENT.into_response()
}

async fn reset(State(state): State<AppState>) -> StatusCode {
    state.conns.lock().await.clear();
    StatusCode::NO_CONTENT
}

async fn sockinfo(State(state): State<AppState>) -> Json<Option<SockInfo>> {
    Json(*state.listener.lock().await)
}

async fn healthz() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok", "spike": "ws-backpressure" }))
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    let arg_num = |name: &str| -> Option<u64> {
        args.iter().position(|a| a == name).map(|i| {
            args.get(i + 1)
                .and_then(|p| p.parse().ok())
                .expect("number")
        })
    };
    let port = arg_num("--port").unwrap_or(9471) as u16;
    // Optional SO_RCVBUF for the listener; accepted sockets inherit it on macOS/BSD.
    let rcvbuf = arg_num("--rcvbuf");

    let state = AppState::default();
    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/ws/ingest", get(ingest))
        .route("/stats", get(stats_all))
        .route("/stats/{id}", get(stats_one))
        .route("/limit", post(set_limit))
        .route("/reset", post(reset))
        .route("/sockinfo", get(sockinfo))
        .fallback_service(ServeDir::new(WEB_DIR))
        .with_state(state.clone());

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("bind listener");
    {
        let sock = socket2::SockRef::from(&listener);
        if let Some(size) = rcvbuf {
            sock.set_recv_buffer_size(size as usize)
                .expect("set SO_RCVBUF");
        }
        let info = SockInfo {
            listener_so_rcvbuf: sock.recv_buffer_size().unwrap_or(0),
            listener_so_sndbuf: sock.send_buffer_size().unwrap_or(0),
        };
        tracing::info!(
            rcvbuf = info.listener_so_rcvbuf,
            sndbuf = info.listener_so_sndbuf,
            "listener socket buffers"
        );
        *state.listener.lock().await = Some(info);
    }
    tracing::info!(%addr, web_dir = WEB_DIR, "ws-backpressure spike listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .expect("server");
}
