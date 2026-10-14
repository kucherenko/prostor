// Spike (c): cost of one JS -> WASM FlvMuxer call in Chrome, copies included.
//
//   just build-wasm && node spikes/rtmp-publish/wasm-overhead.mjs
//
// Serves web/src/lib/wasm/pkg on 127.0.0.1:9473 with a tiny page that calls
// FlvMuxer.video() with a 25 KB payload and FlvMuxer.audio() with 400 bytes, 10 000 times
// each, then prints the result as JSON. Uses the system Chrome through puppeteer-core
// (resolved from web/node_modules).

import http from 'node:http';
import { createRequire } from 'node:module';
import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '../..');
const pkgDir = path.join(root, 'web/src/lib/wasm/pkg');
const require = createRequire(path.join(root, 'web/package.json'));
const puppeteer = require('puppeteer-core');
const port = Number(process.env.PORT ?? 9473);

const page = `<!doctype html><meta charset="utf-8"><title>FlvMuxer overhead</title>
<pre id="out">running...</pre>
<script type="module">
import init, { FlvMuxer } from './prostor_wasm.js';
const ITERATIONS = 10000;
function bench(label, fn) {
  for (let i = 0; i < 500; i++) fn(i);
  const start = performance.now();
  let bytes = 0;
  for (let i = 0; i < ITERATIONS; i++) bytes += fn(i).length;
  const ms = performance.now() - start;
  return { label, iterations: ITERATIONS, totalMs: +ms.toFixed(2), usPerCall: +((ms * 1000) / ITERATIONS).toFixed(3), bytesOut: bytes };
}
try {
  await init();
  const muxer = new FlvMuxer(true, true);
  const video = new Uint8Array(25 * 1024);
  crypto.getRandomValues(video);
  const audio = new Uint8Array(400);
  crypto.getRandomValues(audio);
  const results = [
    bench('wasm FlvMuxer.video, 25 KB payload', (i) => muxer.video(i * 33, i % 60 === 0, 0, video)),
    bench('wasm FlvMuxer.audio, 400 B payload', (i) => muxer.audio(i * 21, audio)),
    bench('js baseline: Uint8Array.slice 25 KB', () => video.slice()),
    bench('js baseline: Uint8Array.slice 400 B', () => audio.slice()),
  ];
  window.__results = { userAgent: navigator.userAgent, hardwareConcurrency: navigator.hardwareConcurrency, results, tagsWritten: muxer.tags_written() };
} catch (error) {
  window.__results = { error: String(error) };
}
document.getElementById('out').textContent = JSON.stringify(window.__results, null, 2);
</script>`;

const types = { '.js': 'text/javascript', '.wasm': 'application/wasm', '.ts': 'text/plain' };
const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, `http://127.0.0.1:${port}`);
  if (url.pathname === '/') {
    res.writeHead(200, { 'content-type': 'text/html; charset=utf-8' });
    return res.end(page);
  }
  try {
    const file = path.join(pkgDir, path.basename(url.pathname));
    const body = await readFile(file);
    res.writeHead(200, { 'content-type': types[path.extname(file)] ?? 'application/octet-stream' });
    res.end(body);
  } catch {
    res.writeHead(404).end('not found');
  }
});
await new Promise((resolve) => server.listen(port, '127.0.0.1', resolve));

const launchOptions = process.env.PROSTOR_CHROME ? { executablePath: process.env.PROSTOR_CHROME } : { channel: 'chrome' };
const browser = await puppeteer.launch({ ...launchOptions, headless: true, args: ['--no-sandbox'] });
try {
  const tab = await browser.newPage();
  tab.on('pageerror', (err) => console.error('pageerror', err.message));
  await tab.goto(`http://127.0.0.1:${port}/`, { waitUntil: 'load' });
  await tab.waitForFunction(() => window.__results !== undefined, { timeout: 120_000 });
  const results = await tab.evaluate(() => window.__results);
  if (results.error) throw new Error(results.error);
  const us = Object.fromEntries(results.results.map((r) => [r.label, r.usPerCall]));
  const perSecondUs = 30 * us['wasm FlvMuxer.video, 25 KB payload'] + 47 * us['wasm FlvMuxer.audio, 400 B payload'];
  results.impliedCostPerSecondAt30fps = { microseconds: +perSecondUs.toFixed(1), percentOfOneCore: +((perSecondUs / 1e6) * 100).toFixed(3) };
  console.log(JSON.stringify(results, null, 2));
} finally {
  await browser.close();
  server.close();
}
