// Scenario runner for the backpressure spike. Builds and starts the Rust sink,
// drives the page in headless Chrome (puppeteer-core, system Chrome), samples
// CPU of the server and of the Chrome process tree, and writes one JSON per
// scenario to ../results/.
//
//   node run.mjs                 # all scenarios
//   node run.mjs --scenario S2   # one (comma-separated list allowed)
//   node run.mjs --no-build --skip-idle --repeat 1

import { spawn, spawnSync } from 'node:child_process';
import { mkdirSync, writeFileSync, openSync } from 'node:fs';
import os from 'node:os';
import { fileURLToPath } from 'node:url';
import puppeteer from 'puppeteer-core';

const CHROME = process.env.PROSTOR_CHROME ?? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const PORT = 9471;
const BASE = `http://127.0.0.1:${PORT}`;
const ROOT = fileURLToPath(new URL('../../../', import.meta.url));
const HERE = fileURLToPath(new URL('./', import.meta.url));
const RESULTS = fileURLToPath(new URL('../results/', import.meta.url));
const SERVER_BIN = `${ROOT}target/release/ws-backpressure`;

const argv = process.argv.slice(2);
const flag = (name) => argv.includes(name);
const opt = (name, d) => {
  const i = argv.indexOf(name);
  return i >= 0 ? argv[i + 1] : d;
};
const only = opt('--scenario', null)?.split(',');
const repeatOverride = opt('--repeat', null);

const SCENARIOS = {
  S1: { desc: 'baseline: localhost, no limit, policy none', page: { policy: 'none' }, repeat: 2 },
  S2: { desc: 'uplink bottleneck 4 Mbit/s (server consumes slower than 6 Mbit/s), policy none', page: { rate_bps: 4e6, policy: 'none' }, repeat: 2 },
  S2b: { desc: 'same bottleneck, server SO_RCVBUF forced to 64 KiB (how much of the hidden buffer is the server side)', page: { rate_bps: 4e6, policy: 'none' }, serverArgs: ['--rcvbuf', '65536'] },
  S3a: { desc: 'bottleneck 4 Mbit/s, policy drop-delta on bufferedAmount > 750 KB, limit lifted at 20 s', page: { rate_bps: 4e6, policy: 'drop-delta', duration: 40 }, liftAtMs: 20_000, repeat: 2 },
  S3b: { desc: 'bottleneck 4 Mbit/s, policy drop-all on bufferedAmount > 750 KB, limit lifted at 20 s', page: { rate_bps: 4e6, policy: 'drop-all', duration: 40 }, liftAtMs: 20_000, repeat: 2 },
  S3c: { desc: 'bottleneck 4 Mbit/s, policy drop-delta on bufferedAmount > 100 KB, limit lifted at 20 s', page: { rate_bps: 4e6, policy: 'drop-delta', threshold: 100_000, duration: 40 }, liftAtMs: 20_000 },
  S3d: { desc: 'bottleneck 4 Mbit/s, policy drop-delta on server ack lag > 500 ms, limit lifted at 20 s', page: { rate_bps: 4e6, policy: 'drop-delta', signal: 'ack', lagMs: 500, duration: 40 }, liftAtMs: 20_000, repeat: 2 },
  S3e: { desc: 'bottleneck 4 Mbit/s, policy drop-all on server ack lag > 500 ms, limit lifted at 20 s', page: { rate_bps: 4e6, policy: 'drop-all', signal: 'ack', lagMs: 500, duration: 40 }, liftAtMs: 20_000 },
  S4: { desc: 'limit 8 Mbit/s (just above average) with keyframe bursts, policy none', page: { rate_bps: 8e6, policy: 'none' } },
  S5a: { desc: 'slow consumer 50 ms per message (20 fps), policy none', page: { slow_ms: 50, policy: 'none' } },
  S5b: { desc: 'slow consumer 50 ms per message, policy drop-delta on bufferedAmount > 750 KB', page: { slow_ms: 50, policy: 'drop-delta' } },
  S5c: { desc: 'slow consumer 50 ms per message, policy drop-delta on ack lag > 500 ms', page: { slow_ms: 50, policy: 'drop-delta', signal: 'ack', lagMs: 500 } },
  S6a: { desc: 'WebSocketStream on the 4 Mbit/s bottleneck, policy none', page: { mode: 'wsstream', rate_bps: 4e6, policy: 'none' } },
  S6b: { desc: 'WebSocketStream on the 4 Mbit/s bottleneck, policy drop-delta', page: { mode: 'wsstream', rate_bps: 4e6, policy: 'drop-delta' } },
  S7: { desc: 'CDP Network.emulateNetworkConditions upload 4 Mbit/s, no server limit', page: { policy: 'none' }, cdpUploadBytesPerSecond: 4e6 / 8 },
  S8a: { desc: 'ceiling: one Node ws client blasting 1 GB', blast: { clients: 1, bytes: 1e9 } },
  S8b: { desc: 'ceiling: four Node ws clients blasting 1 GB each', blast: { clients: 4, bytes: 1e9 } },
  S9: { desc: 'batch=5 frames per message at baseline (compare with S1)', page: { policy: 'none', batch: 5 } },
};

const selected = Object.entries(SCENARIOS).filter(([name]) => !only || only.includes(name));
if (!selected.length) {
  console.error(`unknown scenario(s): ${only}`);
  process.exit(2);
}
mkdirSync(RESULTS, { recursive: true });

// ---------- helpers ----------

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function psSnapshot() {
  const out = spawnSync('ps', ['-A', '-o', 'pid=,ppid=,time=,command='], { encoding: 'utf8' }).stdout;
  const rows = [];
  for (const line of out.split('\n')) {
    const m = line.match(/^\s*(\d+)\s+(\d+)\s+(\S+)\s+(.*)$/);
    if (!m) continue;
    rows.push({ pid: Number(m[1]), ppid: Number(m[2]), cpu: parseCpuTime(m[3]), cmd: m[4] });
  }
  return rows;
}

function parseCpuTime(s) {
  // formats: "0:00.12", "12:34.56", "1:02:03", "1-02:03:04"
  let days = 0;
  if (s.includes('-')) {
    const [d, rest] = s.split('-');
    days = Number(d);
    s = rest;
  }
  const parts = s.split(':').map(Number);
  let secs = 0;
  for (const p of parts) secs = secs * 60 + p;
  return days * 86400 + secs;
}

function descendants(rows, root) {
  const byParent = new Map();
  for (const r of rows) {
    if (!byParent.has(r.ppid)) byParent.set(r.ppid, []);
    byParent.get(r.ppid).push(r.pid);
  }
  const out = new Set([root]);
  const stack = [root];
  while (stack.length) {
    const p = stack.pop();
    for (const c of byParent.get(p) ?? []) {
      if (!out.has(c)) {
        out.add(c);
        stack.push(c);
      }
    }
  }
  return out;
}

/** Samples cumulative CPU time of a set of process trees every `every` ms. */
function startCpuSampler(trees, every = 500) {
  const series = Object.fromEntries(Object.keys(trees).map((k) => [k, []]));
  let prev = null;
  let prevAt = null;
  const timer = setInterval(() => {
    const rows = psSnapshot();
    const now = performance.now();
    const totals = {};
    for (const [name, root] of Object.entries(trees)) {
      const pids = descendants(rows, root);
      totals[name] = rows.filter((r) => pids.has(r.pid)).reduce((a, r) => a + r.cpu, 0);
    }
    if (prev) {
      const dt = (now - prevAt) / 1000;
      for (const name of Object.keys(trees)) {
        series[name].push(Math.round(((totals[name] - prev[name]) / dt) * 100));
      }
    }
    prev = totals;
    prevAt = now;
  }, every);
  return {
    stop() {
      clearInterval(timer);
      const summary = {};
      for (const [name, s] of Object.entries(series)) {
        const body = s.slice(1); // first sample includes startup
        summary[name] = body.length
          ? { meanPercent: Math.round(body.reduce((a, b) => a + b, 0) / body.length), maxPercent: Math.max(...body), samples: body.length }
          : { meanPercent: null, maxPercent: null, samples: 0 };
      }
      return summary;
    },
  };
}

const BUSY = /(^|\/)(cargo|rustc|wasm-pack|wasm-opt|wasm-bindgen)(\s|$)|docker (build|compose)/;
const MAX_LOAD = Number(opt('--max-load', 4));
const IDLE_TIMEOUT_MS = Number(opt('--idle-timeout', 600)) * 1000;
async function waitIdle() {
  const started = performance.now();
  let flagged = false;
  for (;;) {
    const rows = psSnapshot();
    const busy = rows.filter((r) => BUSY.test(r.cmd) && !r.cmd.includes('Docker.app') && r.pid !== process.pid);
    const load = os.loadavg()[0];
    if ((!busy.length && load < MAX_LOAD) || flag('--skip-idle')) {
      return { waitedMs: Math.round(performance.now() - started), loadavg1: load, maxLoad: MAX_LOAD, busy: busy.map((b) => b.cmd.slice(0, 60)), flagged };
    }
    if (performance.now() - started > IDLE_TIMEOUT_MS) {
      flagged = true;
      return { waitedMs: Math.round(performance.now() - started), loadavg1: load, maxLoad: MAX_LOAD, busy: busy.map((b) => b.cmd.slice(0, 60)), flagged };
    }
    console.log(`  waiting for idle machine: load=${load.toFixed(2)} busy=${busy.length}`);
    await sleep(15_000);
  }
}

async function fetchJson(path, init) {
  const r = await fetch(`${BASE}${path}`, init);
  if (!r.ok && r.status !== 204) throw new Error(`${path}: HTTP ${r.status}`);
  return r.status === 204 ? null : r.json();
}

// ---------- server ----------

if (!flag('--no-build')) {
  console.log('building server (release)…');
  const b = spawnSync('cargo', ['build', '-p', 'ws-backpressure', '--release'], { cwd: ROOT, stdio: 'inherit' });
  if (b.status !== 0) process.exit(b.status ?? 1);
}
const serverLog = openSync(`${RESULTS}server.log`, 'w');
let server = null;
let serverArgsKey = null;
let sockinfo = null;
const cleanup = () => {
  if (server && !server.killed) server.kill('SIGTERM');
};
process.on('exit', cleanup);
process.on('SIGINT', () => {
  cleanup();
  process.exit(130);
});

/** (Re)starts the sink when a scenario needs different server arguments. */
async function ensureServer(extraArgs = []) {
  const key = JSON.stringify(extraArgs);
  if (server && key === serverArgsKey) return;
  if (server) {
    server.kill('SIGTERM');
    await new Promise((r) => server.on('exit', r));
    await sleep(300);
  }
  server = spawn(SERVER_BIN, ['--port', String(PORT), ...extraArgs], { cwd: ROOT, stdio: ['ignore', serverLog, serverLog], env: { ...process.env, RUST_LOG: 'info' } });
  serverArgsKey = key;
  for (let i = 0; ; i++) {
    try {
      await fetchJson('/healthz');
      break;
    } catch {
      if (i > 100) {
        console.error('server did not start; see results/server.log');
        process.exit(1);
      }
      await sleep(100);
    }
  }
  sockinfo = await fetchJson('/sockinfo');
  console.log(`server pid ${server.pid} args ${key}, listener buffers ${JSON.stringify(sockinfo)}`);
}
await ensureServer();

// ---------- scenarios ----------

const summary = [];

async function runPageScenario(name, sc, run, browser) {
  const id = `${name.toLowerCase()}-r${run}-${Math.random().toString(36).slice(2, 6)}`;
  const q = new URLSearchParams({ id, ...Object.fromEntries(Object.entries(sc.page).map(([k, v]) => [k, String(v)])) });
  const page = await browser.newPage();
  const pageErrors = [];
  page.on('pageerror', (e) => pageErrors.push(e.message));
  page.on('console', (m) => {
    if (m.type() === 'error') pageErrors.push(m.text());
  });
  if (sc.cdpUploadBytesPerSecond) {
    const cdp = await page.createCDPSession();
    await cdp.send('Network.enable');
    await cdp.send('Network.emulateNetworkConditions', {
      offline: false,
      latency: 0,
      downloadThroughput: -1,
      uploadThroughput: sc.cdpUploadBytesPerSecond,
    });
  }
  const cpu = startCpuSampler({ server: server.pid, chrome: browser.process().pid });
  const t0 = performance.now();
  await page.goto(`${BASE}/?${q}`, { waitUntil: 'load' });
  let lift = null;
  if (sc.liftAtMs) {
    lift = (async () => {
      await sleep(sc.liftAtMs);
      await fetchJson(`/limit?id=${id}&rate_bps=0&slow_ms=0`, { method: 'POST' });
      return Math.round(performance.now() - t0);
    })();
  }
  const duration = sc.page.duration ?? 30;
  await page.waitForFunction(() => window.__results, { timeout: (duration + 90) * 1000, polling: 500 });
  const results = await page.evaluate(() => window.__results);
  const cpuSummary = cpu.stop();
  const liftedAtMs = lift ? await lift : null;
  await page.close();
  return { id, results, cpu: cpuSummary, pageErrors, liftedAtMs };
}

async function runBlastScenario(name, sc) {
  const ids = Array.from({ length: sc.blast.clients }, (_, i) => `${name.toLowerCase()}-${i}`);
  const cpu = startCpuSampler({ server: server.pid }, 250);
  const t0 = performance.now();
  const procs = ids.map((id) =>
    spawn(process.execPath, [`${HERE}blast.mjs`, '--url', `ws://127.0.0.1:${PORT}/ws/ingest?id=${id}`, '--bytes', String(sc.blast.bytes), '--frame', '25000'], { stdio: ['ignore', 'pipe', 'inherit'] }),
  );
  const outputs = await Promise.all(
    procs.map(
      (p) =>
        new Promise((resolve) => {
          let out = '';
          p.stdout.on('data', (d) => (out += d));
          p.on('exit', () => resolve(out.trim()));
        }),
    ),
  );
  const wallMs = performance.now() - t0;
  const cpuSummary = cpu.stop();
  await sleep(300);
  const stats = await Promise.all(ids.map((id) => fetchJson(`/stats/${id}`)));
  const bytes = stats.reduce((a, s) => a + s.bytes, 0);
  return {
    clients: outputs.map((o) => JSON.parse(o || '{}')),
    server: stats.map((s) => ({ id: s.id, bytes: s.bytes, duration_ms: s.duration_ms, mean_bits_per_second: s.mean_bits_per_second, latency_ms: s.latency_ms, frames: s.frames })),
    aggregate: { bytes, wallMs, receiveMbitPerSecond: (bytes * 8) / (wallMs / 1000) / 1e6 },
    cpu: cpuSummary,
  };
}

const browser = await puppeteer.launch({ executablePath: CHROME, headless: true, args: ['--no-sandbox'] });
console.log(`chrome ${await browser.version()} pid ${browser.process().pid}`);

try {
  for (const [name, sc] of selected) {
    const repeat = Number(repeatOverride ?? sc.repeat ?? 1);
    for (let run = 1; run <= repeat; run++) {
      const label = repeat > 1 ? `${name}-run${run}` : name;
      console.log(`\n== ${label}: ${sc.desc}`);
      await ensureServer(sc.serverArgs ?? []);
      const idle = await waitIdle();
      const record = { scenario: name, run, desc: sc.desc, config: sc.page ?? sc.blast, serverArgs: sc.serverArgs ?? [], idle, sockinfo, startedAt: new Date().toISOString() };
      if (sc.blast) {
        Object.assign(record, await runBlastScenario(name, sc));
        const a = record.aggregate;
        console.log(`  receive ${a.receiveMbitPerSecond.toFixed(0)} Mbit/s, server cpu mean ${record.cpu.server.meanPercent}% max ${record.cpu.server.maxPercent}%`);
        summary.push({ scenario: label, receiveMbitPerSecond: +a.receiveMbitPerSecond.toFixed(0), serverCpuMean: record.cpu.server.meanPercent, serverCpuMax: record.cpu.server.maxPercent });
      } else {
        Object.assign(record, await runPageScenario(name, sc, run, browser));
        const r = record.results;
        const c = r.client ?? {};
        const s = r.server ?? {};
        const line = {
          scenario: label,
          sentMbps: c.sentBitsPerSecond != null ? +(c.sentBitsPerSecond / 1e6).toFixed(2) : null,
          recvMbps: s.mean_bits_per_second != null ? +(s.mean_bits_per_second / 1e6).toFixed(2) : null,
          bufferedMax: c.bufferedAmount?.max ?? null,
          bufferedOnsetMs: c.bufferedAmount?.onsetMs ?? null,
          growthBps: c.bufferedAmount?.growthMaxBytesPerSecond != null ? Math.round(c.bufferedAmount.growthMaxBytesPerSecond) : null,
          hiddenMax: c.hiddenBytes?.max ?? null,
          hiddenAtOnset: c.hiddenBytes?.atBufferedOnset ?? null,
          ackLagP95: c.ackLagMs?.p95 ?? null,
          ackLagMax: c.ackLagMs?.max ?? null,
          latMean: s.latency_ms?.mean != null ? +s.latency_ms.mean.toFixed(1) : null,
          latP95: s.latency_ms?.p95 != null ? +s.latency_ms.p95.toFixed(1) : null,
          latMax: s.latency_ms?.max != null ? +s.latency_ms.max.toFixed(0) : null,
          dropped: c.droppedFrames ?? null,
          drainMs: c.drain?.ms != null ? Math.round(c.drain.ms) : null,
          serverCpu: record.cpu.server.meanPercent,
          chromeCpu: record.cpu.chrome.meanPercent,
          jitterP95: c.timerJitterMs?.p95 != null ? +c.timerJitterMs.p95.toFixed(1) : null,
          errors: (c.errors ?? []).concat(record.pageErrors).length,
          load: +idle.loadavg1.toFixed(2),
        };
        console.log('  ' + JSON.stringify(line));
        summary.push(line);
      }
      writeFileSync(`${RESULTS}${label}.json`, JSON.stringify(record, null, 1));
    }
  }
} finally {
  await browser.close();
  cleanup();
}

writeFileSync(`${RESULTS}summary.json`, JSON.stringify(summary, null, 1));
console.log('\nsummary:');
console.table(summary);
