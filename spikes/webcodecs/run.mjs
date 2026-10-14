// Launches the system Chrome via puppeteer-core, runs the spike page, samples the CPU of
// the Chrome process tree per phase and writes results/<timestamp>-<label>.json.
//
//   node run.mjs [--headed] [--gpu-flags] [--label=name] [--no-wait]
//                [--phases=caps,video,audio,clock,camera] [--frames=600] [--seconds=20]
//                [--where=main,worker] [--hw=hw,sw] [--modes=paced,fast]
import { execFileSync } from 'node:child_process';
import { mkdir, writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import puppeteer from 'puppeteer-core';
import { startServer } from './serve.mjs';

const ROOT = dirname(fileURLToPath(import.meta.url));
const CHROME =
  process.env.CHROME_PATH ?? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const PORT = 9472;

const args = Object.fromEntries(
  process.argv.slice(2).map((a) => {
    const m = a.match(/^--([^=]+)(?:=(.*))?$/);
    return m ? [m[1], m[2] ?? true] : [a, true];
  }),
);
const headed = Boolean(args.headed);
const gpuFlags = Boolean(args['gpu-flags']);
const label = args.label ?? (headed ? 'headed' : gpuFlags ? 'headless-gpuflags' : 'headless');

const sh = (cmd, cmdArgs) => {
  try {
    return execFileSync(cmd, cmdArgs, { encoding: 'utf8' }).trim();
  } catch {
    return '';
  }
};

function loadAvg() {
  // "{ 1.23 2.34 3.45 }"
  const m = sh('sysctl', ['-n', 'vm.loadavg']).match(/[\d.]+/g);
  return m ? m.map(Number) : null;
}

function busyProcesses() {
  return sh('pgrep', ['-fl', 'cargo|rustc|wasm-pack|wasm-opt|wasm-bindgen|docker build|buildkit'])
    .split('\n')
    .filter(Boolean);
}

async function waitForIdle(maxMs = 10 * 60_000) {
  const t0 = Date.now();
  for (;;) {
    const busy = busyProcesses();
    const la = loadAvg();
    if (busy.length === 0 && la && la[0] < 4) return { idle: true, waitedMs: Date.now() - t0, loadAvg: la };
    if (Date.now() - t0 > maxMs) return { idle: false, waitedMs: Date.now() - t0, loadAvg: la, busy };
    console.log(`waiting for idle machine: load ${la?.[0]} busy=${busy.length}`);
    await new Promise((r) => setTimeout(r, 15_000));
  }
}

// ---- CPU sampling of a process tree (cumulative CPU time deltas, 100 = one core) ----
function descendants(pid) {
  const out = [pid];
  const stack = [pid];
  while (stack.length) {
    const p = stack.pop();
    for (const c of sh('pgrep', ['-P', String(p)]).split('\n').filter(Boolean).map(Number)) {
      out.push(c);
      stack.push(c);
    }
  }
  return out;
}

function parseCpuTime(s) {
  // "0:01.23" | "12:34.56" | "1:02:03.45"
  return s.split(':').reduce((acc, v) => acc * 60 + Number(v), 0);
}

function snapshot(pids) {
  const rows = sh('ps', ['-o', 'pid=,time=,rss=', '-p', pids.join(',')])
    .split('\n')
    .filter(Boolean)
    .map((l) => l.trim().split(/\s+/));
  const cpu = new Map();
  let rssKb = 0;
  for (const [pid, time, rss] of rows) {
    cpu.set(Number(pid), parseCpuTime(time));
    rssKb += Number(rss);
  }
  return { at: performance.now(), cpu, rssKb };
}

function startSampler(rootPid, phaseRef, intervalMs = 1000) {
  const samples = [];
  let prev = snapshot(descendants(rootPid));
  const timer = setInterval(() => {
    const cur = snapshot(descendants(rootPid));
    let delta = 0;
    for (const [pid, t] of cur.cpu) {
      const before = prev.cpu.get(pid);
      if (before !== undefined) delta += Math.max(0, t - before);
    }
    const pct = (delta / ((cur.at - prev.at) / 1000)) * 100;
    samples.push({ phase: phaseRef.current, pct, rssMb: cur.rssKb / 1024 });
    prev = cur;
  }, intervalMs);
  return {
    stop() {
      clearInterval(timer);
      const byPhase = {};
      for (const s of samples) {
        const b = (byPhase[s.phase ?? 'idle'] ??= { samples: 0, sum: 0, max: 0, rssSum: 0 });
        b.samples++;
        b.sum += s.pct;
        b.max = Math.max(b.max, s.pct);
        b.rssSum += s.rssMb;
      }
      const phases = Object.fromEntries(
        Object.entries(byPhase).map(([k, b]) => [
          k,
          { samples: b.samples, meanPct: Math.round(b.sum / b.samples), maxPct: Math.round(b.max), meanRssMb: Math.round(b.rssSum / b.samples) },
        ]),
      );
      return { intervalMs, phases };
    },
  };
}

async function gpuInfo(browser) {
  try {
    const page = await browser.newPage();
    await page.goto('chrome://gpu', { waitUntil: 'load', timeout: 15_000 });
    await new Promise((r) => setTimeout(r, 1500));
    const text = await page.evaluate(() => {
      const grab = (node) =>
        (node.textContent ?? '') +
        Array.from(node.querySelectorAll('*'))
          .filter((e) => e.shadowRoot)
          .map((e) => '\n' + grab(e.shadowRoot))
          .join('');
      return grab(document.body);
    });
    await page.close();
    const lines = [...new Set(text.split('\n').map((l) => l.trim()).filter(Boolean))];
    return lines.filter((l) => /video (en|de)code|webgl|metal|gl_renderer|gpu process|hardware accelerated|angle|software only|disabled/i.test(l)).slice(0, 40);
  } catch (e) {
    return [`unavailable: ${e}`];
  }
}

async function main() {
  const idle = args['no-wait'] ? { idle: null, waitedMs: 0, loadAvg: loadAvg() } : await waitForIdle();
  const server = await startServer(PORT);
  const launchArgs = [
    '--use-fake-device-for-media-stream=fps=30',
    '--use-fake-ui-for-media-stream',
    '--autoplay-policy=no-user-gesture-required',
    '--no-first-run',
    '--no-default-browser-check',
    '--disable-extensions',
    '--window-size=1280,900',
    ...(gpuFlags ? ['--enable-gpu', '--ignore-gpu-blocklist', '--enable-gpu-rasterization'] : []),
    ...(args.features ? [`--enable-features=${args.features}`] : []),
    ...(args['blink-features'] ? [`--enable-blink-features=${args['blink-features']}`] : []),
  ];
  const browser = await puppeteer.launch({
    executablePath: CHROME,
    headless: !headed,
    args: launchArgs,
    defaultViewport: null,
  });
  const chromeVersion = await browser.version();
  const gpu = await gpuInfo(browser);

  const page = await browser.newPage();
  const phaseRef = { current: 'setup' };
  await page.exposeFunction('__phase', (name, state) => {
    phaseRef.current = state === 'start' ? name : 'between';
  });
  page.on('console', (msg) => {
    const t = msg.text();
    if (t.startsWith('[spike]')) console.log(t);
  });
  page.on('pageerror', (e) => console.error('pageerror', e));

  const query = new URLSearchParams({ auto: '1' });
  for (const k of ['phases', 'frames', 'seconds', 'where', 'hw', 'modes', 'sources', 'latency', 'maxq']) if (args[k]) query.set(k, String(args[k]));
  const url = `http://127.0.0.1:${PORT}/index.html?${query}`;
  const loadBefore = loadAvg();
  const sampler = startSampler(browser.process().pid, phaseRef);
  const t0 = Date.now();
  await page.goto(url);
  // Poll instead of waitForFunction so a crash or a closed window still leaves the
  // latest partial results on disk.
  let results = null;
  let failure = null;
  const deadline = Date.now() + 25 * 60_000;
  while (Date.now() < deadline) {
    try {
      const snap = await page.evaluate('({ done: window.__done, results: window.__results })');
      if (snap?.results) results = snap.results;
      if (snap?.done) break;
    } catch (e) {
      failure = String(e);
      break;
    }
    await new Promise((r) => setTimeout(r, 2000));
  }
  if (!failure && !results?.meta?.finishedAt) failure = 'timeout';
  const cpu = sampler.stop();
  const loadAfter = loadAvg();

  try {
    await browser.close();
  } catch {
    /* already gone */
  }
  server.close();
  if (failure) console.error(`run ended early: ${failure} (partial results saved)`);

  const out = {
    meta: {
      label,
      headed,
      gpuFlags,
      chromeVersion,
      chromePath: CHROME,
      launchArgs,
      os: `${sh('sw_vers', ['-productName'])} ${sh('sw_vers', ['-productVersion'])}`,
      cpu: sh('sysctl', ['-n', 'machdep.cpu.brand_string']),
      cores: Number(sh('sysctl', ['-n', 'hw.ncpu'])),
      idleWait: idle,
      loadAvgBefore: loadBefore,
      loadAvgAfter: loadAfter,
      totalMs: Date.now() - t0,
      date: new Date().toISOString(),
      failure,
    },
    gpu,
    cpu,
    results: results ?? {},
  };
  const dir = join(ROOT, 'results');
  await mkdir(dir, { recursive: true });
  const file = join(dir, `${out.meta.date.replace(/[:.]/g, '-')}-${label}.json`);
  await writeFile(file, JSON.stringify(out, null, 1));
  printSummary(out);
  console.log(`\nwrote ${file}`);
}

function printSummary(out) {
  const r = out.results;
  console.log(`\n== ${out.meta.label} (${out.meta.chromeVersion}) load before/after ${out.meta.loadAvgBefore?.[0]} / ${out.meta.loadAvgAfter?.[0]}`);
  console.log('gpu:', out.gpu.filter((l) => /video encode|video decode|webgl:|gl_renderer/i.test(l)).join(' | '));
  if (r.caps) {
    const sup = r.caps.video.filter((v) => v.supported).map((v) => `${v.codec}/${v.hw}/${v.bitrateMode ?? v.format}`);
    console.log(`video caps supported (${sup.length}/${r.caps.video.length}):`, sup.join(', '));
    console.log('audio caps:', r.caps.audio.map((a) => `${a.codec}${a.format ? '/' + a.format : ''}=${a.supported}`).join(', '));
  }
  for (const [k, v] of Object.entries(r.video ?? {})) {
    if (!v.supported) {
      console.log(`${k}: unsupported ${v.error ?? ''}`);
      continue;
    }
    if (v.error) {
      console.log(`${k}: error ${v.error}`);
      continue;
    }
    console.log(
      `${k}: ${v.framesOut}/${v.framesIn} frames (drop ${v.dropped}) ${v.fps} fps, loop ${v.loopMs} + flush ${v.flushMs} ms, lat mean ${v.encodeLatencyMs.mean} p95 ${v.encodeLatencyMs.p95} max ${v.encodeLatencyMs.max} ms, draw ${v.drawMs.mean} ms, q max ${v.queueMax} (>2: ${v.queueOver2}), ${v.kbps} kbps, key ${v.keyframes.count}x~${v.keyframes.mean}B, P ~${v.pframes.mean}B, desc ${v.description?.length}B first=${v.description?.arrivedWithFirstChunk}, tsMismatch ${v.tsMismatches}, cpu ${out.cpu.phases[`video:${k}`]?.meanPct}%`,
    );
  }
  for (const [k, v] of Object.entries(r.audio ?? {})) {
    console.log(`audio ${k}: supported=${v.supported} chunks=${v.chunks} ${v.kbps} kbps desc=${v.description?.hex} durations=${JSON.stringify(v.durationsUs)} preserved=${v.outputTimestampsPreserved} drift=${v.endDriftUs}`);
  }
  if (r.clock) console.log('clock:', JSON.stringify(r.clock));
  if (r.camera) {
    console.log('camera source:', JSON.stringify(r.camera.sourceSettings && { w: r.camera.sourceSettings.width, h: r.camera.sourceSettings.height, fps: r.camera.sourceSettings.frameRate }));
    console.log('camera trackTransfer:', JSON.stringify(r.camera.trackTransfer));
    console.log('camera frameTransfer:', JSON.stringify(r.camera.frameTransfer));
  }
  if (r.errors?.length) console.log('errors:', JSON.stringify(r.errors));
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
