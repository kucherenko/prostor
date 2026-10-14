// Browser side of the backpressure spike. Sends synthetic key/delta frames at a
// target bitrate over `WebSocket` (or `WebSocketStream`), samples
// `bufferedAmount`, applies a drop policy and correlates with the server's
// `/stats` to find out how many bytes hide between the browser and the server.
//
// Record layout (little-endian): u32 record length, u8 kind (1 key, 2 delta),
// u8 pad, u16 pad, u32 seq, f64 Date.now() ms, payload.

const params = new URLSearchParams(location.search);
const num = (k, d) => (params.get(k) == null ? d : Number(params.get(k)));
const str = (k, d) => params.get(k) ?? d;

const cfg = {
  id: str('id', Math.random().toString(36).slice(2, 10)),
  bitrate: num('bitrate', 6_000_000),
  fps: num('fps', 30),
  gop: num('gop', 2),
  keyRatio: num('keyRatio', 6),
  duration: num('duration', 30),
  policy: str('policy', 'none'),
  threshold: num('threshold', 0),
  mode: str('mode', 'ws'),
  batch: Math.max(1, num('batch', 1)),
  rate_bps: num('rate_bps', 0),
  slow_ms: num('slow_ms', 0),
  // Pressure signal for the drop policy: `buffered` (ws.bufferedAmount above
  // `threshold`) or `ack` (age of the newest frame the server has consumed,
  // as reported by its periodic ack, above `lagMs`).
  signal: str('signal', 'buffered'),
  lagMs: num('lagMs', 500),
  ack_ms: num('ack_ms', 100),
  statsEvery: num('statsEvery', 500),
  sampleEvery: num('sampleEvery', 50),
  drainTimeout: num('drainTimeout', 60),
};
if (!cfg.threshold) cfg.threshold = Math.round(cfg.bitrate / 8); // one second of video

const gopFrames = cfg.fps * cfg.gop;
const avgBytes = cfg.bitrate / 8 / cfg.fps;
const deltaBytes = Math.round((avgBytes * gopFrames) / (cfg.keyRatio + gopFrames - 1));
const keyBytes = Math.round(deltaBytes * cfg.keyRatio);
const sizes = { gopFrames, avgBytes, deltaBytes, keyBytes };

const $ = (id) => document.getElementById(id);
$('config').textContent = JSON.stringify({ ...cfg, ...sizes });
const setStatus = (s) => ($('status').textContent = s);

// getRandomValues is capped at 64 KiB per call; fill in chunks.
const random = new Uint8Array(keyBytes + 64);
for (let off = 0; off < random.length; off += 65536) {
  crypto.getRandomValues(random.subarray(off, Math.min(off + 65536, random.length)));
}

function makeRecord(kind, seq, size) {
  const buf = new ArrayBuffer(4 + 16 + size);
  const dv = new DataView(buf);
  dv.setUint32(0, 16 + size, true);
  dv.setUint8(4, kind);
  dv.setUint32(8, seq, true);
  dv.setFloat64(12, Date.now(), true);
  new Uint8Array(buf, 20, size).set(random.subarray(0, size));
  return buf;
}

function concat(buffers) {
  const total = buffers.reduce((n, b) => n + b.byteLength, 0);
  const out = new Uint8Array(total);
  let off = 0;
  for (const b of buffers) {
    out.set(new Uint8Array(b), off);
    off += b.byteLength;
  }
  return out.buffer;
}

const wsUrl = `ws://${location.host}/ws/ingest?id=${cfg.id}&rate_bps=${cfg.rate_bps}&slow_ms=${cfg.slow_ms}&ack_ms=${cfg.ack_ms}`;

// Ack state: the server periodically reports the newest consumed frame. The
// pipe's delay is the age of the oldest frame the server has NOT consumed yet
// (zero when everything sent has been acked). Using the age of the newest
// acked frame instead deadlocks a drop-all policy: once sending stops, the
// server has nothing to ack and the stale age grows forever.
const ack = { seq: -1, sentMs: 0, receivedAt: 0, count: 0 };
const outstanding = []; // [seq, Date.now() at send], in send order
function onAck(text) {
  try {
    const a = JSON.parse(text);
    ack.seq = a.seq;
    ack.sentMs = a.sent_ms;
    ack.receivedAt = Date.now();
    ack.count += 1;
    while (outstanding.length && outstanding[0][0] <= a.seq) outstanding.shift();
  } catch (e) {
    errors.push(`ack parse: ${e?.message ?? e}`);
  }
}
const ackLag = () => (outstanding.length ? Date.now() - outstanding[0][1] : 0);

// Transport abstraction: classic WebSocket vs WebSocketStream.
async function openTransport() {
  if (cfg.mode === 'wsstream') {
    if (typeof WebSocketStream === 'undefined') throw new Error('WebSocketStream unavailable');
    const wss = new WebSocketStream(wsUrl);
    const { writable, readable } = await wss.opened;
    const writer = writable.getWriter();
    (async () => {
      const reader = readable.getReader();
      for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        if (typeof value === 'string') onAck(value);
      }
    })().catch(() => {});
    let unresolvedBytes = 0;
    let pending = 0;
    let pendingMax = 0;
    const writeMs = [];
    const desired = [];
    return {
      kind: 'wsstream',
      // The only backpressure signal here is the write promise / desiredSize.
      buffered: () => unresolvedBytes,
      pressure: () =>
        cfg.signal === 'ack' ? ackLag() > cfg.lagMs : pending > 0 || (writer.desiredSize ?? 1) <= 0,
      send(buf) {
        const t0 = performance.now();
        unresolvedBytes += buf.byteLength;
        pending += 1;
        pendingMax = Math.max(pendingMax, pending);
        desired.push(writer.desiredSize);
        writer.write(new Uint8Array(buf)).then(
          () => {
            unresolvedBytes -= buf.byteLength;
            pending -= 1;
            writeMs.push(performance.now() - t0);
          },
          (e) => {
            errors.push(`write: ${e?.message ?? e}`);
            pending -= 1;
          },
        );
      },
      async close() {
        try {
          await writer.close();
        } catch (e) {
          errors.push(`close: ${e?.message ?? e}`);
        }
        const closed = await wss.closed.catch((e) => ({ closeCode: -1, reason: String(e) }));
        return { closeCode: closed?.closeCode, reason: closed?.reason };
      },
      extra: () => ({
        pendingMax,
        writeMs: summarize(writeMs),
        desiredSizeMin: desired.length ? Math.min(...desired.filter((d) => d != null)) : null,
      }),
    };
  }

  const ws = new WebSocket(wsUrl);
  ws.binaryType = 'arraybuffer';
  await new Promise((resolve, reject) => {
    ws.onopen = resolve;
    ws.onerror = () => reject(new Error('websocket error on open'));
  });
  let closeInfo = null;
  const closed = new Promise((resolve) => {
    ws.onclose = (e) => {
      closeInfo = { closeCode: e.code, reason: e.reason, wasClean: e.wasClean };
      resolve(closeInfo);
    };
  });
  ws.onerror = () => errors.push('websocket error');
  ws.onmessage = (e) => {
    if (typeof e.data === 'string') onAck(e.data);
  };
  return {
    kind: 'ws',
    buffered: () => ws.bufferedAmount,
    pressure: () => (cfg.signal === 'ack' ? ackLag() > cfg.lagMs : ws.bufferedAmount > cfg.threshold),
    send: (buf) => ws.send(buf),
    async close() {
      ws.close(1000, 'done');
      return closed;
    },
    extra: () => ({}),
  };
}

function summarize(values) {
  if (!values.length) return { count: 0, mean: 0, p50: 0, p95: 0, max: 0 };
  const sorted = [...values].sort((a, b) => a - b);
  const pick = (q) => sorted[Math.round((sorted.length - 1) * q)];
  return {
    count: sorted.length,
    mean: sorted.reduce((a, b) => a + b, 0) / sorted.length,
    p50: pick(0.5),
    p95: pick(0.95),
    max: sorted[sorted.length - 1],
  };
}

const errors = [];
const samples = []; // [t ms, bufferedAmount]
const hidden = []; // correlation with the server
const jitter = [];
let sentFrames = 0;
let sentBytes = 0;
let droppedFrames = 0;
let droppedBytes = 0;
let droppedKey = 0;
let dropping = false;
let decisions = [];

async function run() {
  setStatus('connecting');
  const transport = await openTransport();
  setStatus('sending');

  const t0 = performance.now();
  const started = Date.now();
  let seq = 0;
  let nextT = t0;
  let batchBuf = [];
  let stopped = false;

  const flush = () => {
    if (!batchBuf.length) return;
    const msg = batchBuf.length === 1 ? batchBuf[0] : concat(batchBuf);
    transport.send(msg);
    batchBuf = [];
  };

  const sampler = setInterval(() => {
    samples.push([Math.round(performance.now() - t0), transport.buffered(), ackLag()]);
  }, cfg.sampleEvery);

  const statsPoll = setInterval(async () => {
    const t = Math.round(performance.now() - t0);
    const sent = sentBytes;
    const buffered = transport.buffered();
    try {
      const s = await fetch(`/stats/${cfg.id}`).then((r) => r.json());
      const lastSec = s.latency_by_second?.at(-1);
      hidden.push({
        t,
        sentBytes: sent,
        buffered,
        serverBytes: s.bytes,
        hiddenBytes: sent - buffered - s.bytes,
        serverLatencyMeanMs: lastSec?.mean_ms ?? null,
        serverLatencyMaxMs: lastSec?.max_ms ?? null,
        serverRateBps: s.rate_bps,
      });
    } catch (e) {
      errors.push(`stats: ${e?.message ?? e}`);
    }
  }, cfg.statsEvery);

  await new Promise((resolve) => {
    const tick = () => {
      const now = performance.now();
      jitter.push(now - nextT);
      const kind = seq % gopFrames === 0 ? 1 : 2;
      const size = kind === 1 ? keyBytes : deltaBytes;
      const buffered = transport.buffered();
      const pressure = transport.pressure();

      let send = true;
      if (cfg.policy === 'drop-delta') {
        send = !(pressure && kind === 2);
      } else if (cfg.policy === 'drop-all') {
        if (!dropping && pressure) dropping = true;
        if (dropping) {
          // Resume at a keyframe once the pipe has drained: everything sent is
          // acked (ack signal) or bufferedAmount fell below half the threshold.
          const relieved =
            cfg.signal === 'ack'
              ? outstanding.length === 0
              : transport.kind === 'ws'
                ? buffered < cfg.threshold / 2
                : !pressure;
          if (relieved && kind === 1) dropping = false;
          send = !dropping;
        }
      }

      if (send) {
        batchBuf.push(makeRecord(kind, seq, size));
        outstanding.push([seq, Date.now()]);
        sentFrames += 1;
        sentBytes += 4 + 16 + size;
        if (batchBuf.length >= cfg.batch) flush();
      } else {
        droppedFrames += 1;
        droppedBytes += 4 + 16 + size;
        if (kind === 1) droppedKey += 1;
        if (decisions.length < 2000) decisions.push([Math.round(now - t0), seq, buffered]);
      }
      seq += 1;

      if (seq % cfg.fps === 0) {
        $('live').textContent = `t=${((now - t0) / 1000).toFixed(0)}s sent=${sentFrames} dropped=${droppedFrames} buffered=${buffered} ackLag=${ackLag()}ms`;
      }

      if (now - t0 >= cfg.duration * 1000) {
        flush();
        stopped = true;
        resolve();
        return;
      }
      nextT += 1000 / cfg.fps;
      setTimeout(tick, Math.max(0, nextT - performance.now()));
    };
    tick();
  });

  setStatus('draining');
  const drainStart = performance.now();
  while (transport.buffered() > 0 && performance.now() - drainStart < cfg.drainTimeout * 1000) {
    await new Promise((r) => setTimeout(r, 20));
  }
  const drainMs = performance.now() - drainStart;
  const drained = transport.buffered() === 0;
  clearInterval(sampler);
  clearInterval(statsPoll);

  setStatus('closing');
  const close = await transport.close();
  await new Promise((r) => setTimeout(r, 300));
  let server = null;
  try {
    server = await fetch(`/stats/${cfg.id}`).then((r) => r.json());
  } catch (e) {
    errors.push(`final stats: ${e?.message ?? e}`);
  }

  // Client-side summaries.
  const lagSeries = samples.map((s) => s[2]);
  const lagAfterWarmup = samples.filter((s) => s[0] > 1000).map((s) => s[2]);
  const ba = samples.map((s) => s[1]);
  const baMax = Math.max(0, ...ba);
  const baMean = ba.length ? ba.reduce((a, b) => a + b, 0) / ba.length : 0;
  const aboveMs = samples.filter((s) => s[1] > cfg.threshold).length * cfg.sampleEvery;
  // Onset of visible backpressure: more than two keyframes queued (a single
  // keyframe transiently exceeds 64 KB on every GOP boundary).
  const onset = samples.find((s) => s[1] > 2 * keyBytes)?.[0] ?? null;
  // Max growth over any 5 s window (bytes/s).
  const win = Math.round(5000 / cfg.sampleEvery);
  let growthMax = 0;
  for (let i = win; i < samples.length; i++) {
    const g = (samples[i][1] - samples[i - win][1]) / 5;
    if (g > growthMax) growthMax = g;
  }
  const hiddenMax = hidden.length ? Math.max(...hidden.map((h) => h.hiddenBytes)) : null;
  const hiddenSteady = hidden.filter((h) => h.t > 10_000);
  const hiddenMean = hiddenSteady.length
    ? hiddenSteady.reduce((a, h) => a + h.hiddenBytes, 0) / hiddenSteady.length
    : null;

  window.__results = {
    cfg,
    sizes,
    started,
    userAgent: navigator.userAgent,
    hardwareConcurrency: navigator.hardwareConcurrency,
    transport: transport.kind,
    webSocketStreamAvailable: typeof WebSocketStream !== 'undefined',
    client: {
      sentFrames,
      sentBytes,
      sentBitsPerSecond: (sentBytes * 8) / cfg.duration,
      droppedFrames,
      droppedBytes,
      droppedKeyFrames: droppedKey,
      bufferedAmount: { max: baMax, mean: baMean, msAboveThreshold: aboveMs, onsetMs: onset, growthMaxBytesPerSecond: growthMax },
      hiddenBytes: { max: hiddenMax, meanAfter10s: hiddenMean, atBufferedOnset: onset != null ? (hidden.find((h) => h.t >= onset)?.hiddenBytes ?? null) : null },
      ackLagMs: { ...summarize(lagAfterWarmup), acks: ack.count, last: lagSeries.at(-1) ?? null, outstandingAtEnd: outstanding.length },
      drain: { ms: drainMs, drained },
      timerJitterMs: summarize(jitter),
      close,
      errors,
      ...transport.extra(),
    },
    server,
    samples,
    hidden,
    dropDecisions: decisions,
  };
  $('out').textContent = JSON.stringify({ ...window.__results, samples: `${samples.length} samples`, hidden: `${hidden.length} points`, dropDecisions: decisions.length }, null, 2);
  setStatus('done');
}

window.addEventListener('error', (e) => {
  if (!window.__results) {
    window.__results = { cfg, sizes, error: String(e.message), client: { errors: [String(e.message)] } };
    setStatus('failed');
  }
});

run().catch((e) => {
  errors.push(`fatal: ${e?.message ?? e}`);
  window.__results = { cfg, sizes, error: String(e), client: { errors } };
  $('out').textContent = String(e);
  setStatus('failed');
});
