// Shared measurement code. Runs unchanged in the window and inside a Worker.
// Everything returns plain JSON-friendly objects; no per-frame arrays leak out.

export const FPS = 30;
export const FRAME_US = Math.round(1e6 / FPS); // 33333

export function sleep(ms) {
  return new Promise((r) => setTimeout(r, ms));
}

export function summarize(values) {
  if (!values.length) return { n: 0 };
  const sorted = [...values].sort((a, b) => a - b);
  const pick = (q) => sorted[Math.min(sorted.length - 1, Math.floor(q * sorted.length))];
  const sum = sorted.reduce((a, b) => a + b, 0);
  return {
    n: sorted.length,
    min: round(sorted[0]),
    mean: round(sum / sorted.length),
    p50: round(pick(0.5)),
    p95: round(pick(0.95)),
    max: round(sorted[sorted.length - 1]),
  };
}

export function round(x, digits = 2) {
  const f = 10 ** digits;
  return Math.round(x * f) / f;
}

export function bytesOf(bufferSource) {
  if (!bufferSource) return null;
  if (bufferSource instanceof ArrayBuffer) return new Uint8Array(bufferSource);
  return new Uint8Array(bufferSource.buffer, bufferSource.byteOffset, bufferSource.byteLength);
}

export function hex(u8, n = 16) {
  if (!u8) return null;
  return Array.from(u8.slice(0, n), (b) => b.toString(16).padStart(2, '0')).join(' ');
}

export function nowAbs() {
  // Comparable across window and worker (different performance time origins).
  return performance.timeOrigin + performance.now();
}

export function envProbe() {
  const t = (name) => typeof globalThis[name];
  return {
    isWorker: typeof WorkerGlobalScope !== 'undefined',
    VideoEncoder: t('VideoEncoder'),
    VideoDecoder: t('VideoDecoder'),
    AudioEncoder: t('AudioEncoder'),
    VideoFrame: t('VideoFrame'),
    AudioData: t('AudioData'),
    OffscreenCanvas: t('OffscreenCanvas'),
    MediaStreamTrackProcessor: t('MediaStreamTrackProcessor'),
    MediaStreamTrackGenerator: t('MediaStreamTrackGenerator'),
    VideoTrackGenerator: t('VideoTrackGenerator'),
    WebSocketStream: t('WebSocketStream'),
    WebTransport: t('WebTransport'),
    navigatorGpu: typeof navigator.gpu,
    hardwareConcurrency: navigator.hardwareConcurrency,
    userAgent: navigator.userAgent,
  };
}

// ---------- capability matrix ----------

const H264 = [
  ['avc1.420028', 'baseline 4.0'],
  ['avc1.4d0028', 'main 4.0'],
  ['avc1.640028', 'high 4.0'],
];
const HW = ['no-preference', 'prefer-hardware', 'prefer-software'];
const BITRATE_MODES = ['constant', 'variable'];

export async function capabilityMatrix() {
  const video = [];
  for (const [codec, name] of H264) {
    for (const hw of HW) {
      for (const bitrateMode of BITRATE_MODES) {
        const config = {
          codec,
          width: 1920,
          height: 1080,
          framerate: 30,
          bitrate: 6_000_000,
          hardwareAcceleration: hw,
          latencyMode: 'realtime',
          bitrateMode,
          avc: { format: 'avc' },
        };
        video.push(await probeVideo(config, name));
      }
    }
  }
  video.push(
    await probeVideo(
      {
        codec: 'avc1.4d0028',
        width: 1920,
        height: 1080,
        framerate: 30,
        bitrate: 6_000_000,
        hardwareAcceleration: 'no-preference',
        latencyMode: 'realtime',
        bitrateMode: 'constant',
        avc: { format: 'annexb' },
      },
      'main 4.0 (annexb)',
    ),
  );
  // Level 3.1 caps at 1280x720@30: expected to be rejected at 1080p30 (level validation).
  video.push(
    await probeVideo(
      {
        codec: 'avc1.42001f',
        width: 1920,
        height: 1080,
        framerate: 30,
        bitrate: 6_000_000,
        hardwareAcceleration: 'no-preference',
        latencyMode: 'realtime',
        bitrateMode: 'constant',
        avc: { format: 'avc' },
      },
      'baseline 3.1 (expected unsupported at 1080p)',
    ),
  );
  // Informational: other codecs the platform offers at 1080p30.
  for (const codec of ['vp09.00.40.08', 'av01.0.08M.08', 'hvc1.1.6.L120.B0']) {
    video.push(
      await probeVideo(
        {
          codec,
          width: 1920,
          height: 1080,
          framerate: 30,
          bitrate: 6_000_000,
          hardwareAcceleration: 'no-preference',
          latencyMode: 'realtime',
        },
        codec,
      ),
    );
  }

  const audio = [];
  const audioConfigs = [
    { codec: 'mp4a.40.2', aac: { format: 'aac' } },
    { codec: 'mp4a.40.2', aac: { format: 'adts' } },
    { codec: 'mp4a.40.5' },
    { codec: 'opus' },
  ];
  for (const extra of audioConfigs) {
    const config = { sampleRate: 48000, numberOfChannels: 2, bitrate: 128_000, ...extra };
    try {
      const res = await AudioEncoder.isConfigSupported(config);
      audio.push({ codec: config.codec, format: extra.aac?.format ?? null, supported: res.supported, config: res.config });
    } catch (e) {
      audio.push({ codec: config.codec, format: extra.aac?.format ?? null, supported: false, error: String(e) });
    }
  }
  return { video, audio };
}

async function probeVideo(config, name) {
  const row = {
    codec: config.codec,
    name,
    hw: config.hardwareAcceleration,
    bitrateMode: config.bitrateMode ?? null,
    format: config.avc?.format ?? null,
  };
  try {
    const res = await VideoEncoder.isConfigSupported(config);
    row.supported = res.supported;
    row.config = res.config;
  } catch (e) {
    row.supported = false;
    row.error = String(e);
  }
  return row;
}

// ---------- frame source ----------

function mulberry32(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

// source: 'canvas2d' (GPU-backed 2d canvas, default) | 'canvas2d-cpu' (willReadFrequently,
// software raster) | 'rgba' (getImageData → CPU VideoFrame) | 'webgl2' (the compositor path).
export function makeFrameSource(width, height, source = 'canvas2d') {
  if (source === 'webgl2') return makeWebglSource(width, height);
  const canvas = new OffscreenCanvas(width, height);
  const ctx = canvas.getContext('2d', { alpha: false, willReadFrequently: source === 'canvas2d-cpu' });
  const rng = mulberry32(42);
  return {
    draw(i, timestampUs = Math.round((i * 1e6) / FPS)) {
      const t = i / FPS;
      const g = ctx.createLinearGradient(0, 0, width, height);
      g.addColorStop(0, `hsl(${(t * 40) % 360} 70% 40%)`);
      g.addColorStop(1, `hsl(${(t * 40 + 120) % 360} 70% 30%)`);
      ctx.fillStyle = g;
      ctx.fillRect(0, 0, width, height);
      for (let k = 0; k < 40; k++) {
        ctx.fillStyle = `hsl(${(k * 37 + t * 90) % 360} 80% 55%)`;
        const x = (Math.sin(t * 0.7 + k) * 0.5 + 0.5) * width;
        const y = (Math.cos(t * 0.9 + k * 1.3) * 0.5 + 0.5) * height;
        ctx.fillRect(x, y, 80 + k * 3, 60 + k * 2);
      }
      // A strip of random blocks: real entropy so the encoder cannot cheat.
      for (let k = 0; k < 200; k++) {
        ctx.fillStyle = `rgb(${(rng() * 255) | 0} ${(rng() * 255) | 0} ${(rng() * 255) | 0})`;
        ctx.fillRect(rng() * width, height - 120 + rng() * 108, 12, 12);
      }
      ctx.fillStyle = '#fff';
      ctx.font = 'bold 72px sans-serif';
      ctx.fillText(`prostor spike frame ${i} t=${t.toFixed(2)}s`, 60, 120);
      if (source === 'rgba') {
        const img = ctx.getImageData(0, 0, width, height);
        return new VideoFrame(img.data, {
          format: 'RGBA',
          codedWidth: width,
          codedHeight: height,
          timestamp: timestampUs,
          duration: FRAME_US,
        });
      }
      return new VideoFrame(canvas, { timestamp: timestampUs, duration: FRAME_US });
    },
  };
}

function makeWebglSource(width, height) {
  const canvas = new OffscreenCanvas(width, height);
  const gl = canvas.getContext('webgl2', { alpha: false, antialias: false, preserveDrawingBuffer: false });
  if (!gl) throw new Error('webgl2 unavailable');
  const vsSrc = `#version 300 es
    in vec2 p; void main() { gl_Position = vec4(p, 0.0, 1.0); }`;
  const fsSrc = `#version 300 es
    precision highp float; uniform float t; uniform vec2 res; out vec4 o;
    float hash(vec2 q) { return fract(sin(dot(q, vec2(12.9898, 78.233))) * 43758.5453); }
    void main() {
      vec2 uv = gl_FragCoord.xy / res;
      vec3 col = 0.5 + 0.5 * cos(t * 0.5 + uv.xyx * 3.0 + vec3(0.0, 2.0, 4.0));
      float strip = step(0.89, uv.y);
      float noise = hash(floor(gl_FragCoord.xy / 8.0) + vec2(t, -t));
      float blob = smoothstep(0.2, 0.19, distance(uv * vec2(res.x / res.y, 1.0), vec2(0.9 + 0.6 * sin(t), 0.5 + 0.3 * cos(t * 1.3))));
      o = vec4(mix(col, vec3(noise), strip) + blob * 0.5, 1.0);
    }`;
  const compile = (type, src) => {
    const s = gl.createShader(type);
    gl.shaderSource(s, src);
    gl.compileShader(s);
    if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s));
    return s;
  };
  const prog = gl.createProgram();
  gl.attachShader(prog, compile(gl.VERTEX_SHADER, vsSrc));
  gl.attachShader(prog, compile(gl.FRAGMENT_SHADER, fsSrc));
  gl.linkProgram(prog);
  if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(prog));
  gl.useProgram(prog);
  const buf = gl.createBuffer();
  gl.bindBuffer(gl.ARRAY_BUFFER, buf);
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 3, -1, -1, 3]), gl.STATIC_DRAW);
  const loc = gl.getAttribLocation(prog, 'p');
  gl.enableVertexAttribArray(loc);
  gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);
  const tLoc = gl.getUniformLocation(prog, 't');
  const resLoc = gl.getUniformLocation(prog, 'res');
  gl.viewport(0, 0, width, height);
  return {
    draw(i, timestampUs = Math.round((i * 1e6) / FPS)) {
      gl.uniform1f(tLoc, i / FPS);
      gl.uniform2f(resLoc, width, height);
      gl.drawArrays(gl.TRIANGLES, 0, 3);
      return new VideoFrame(canvas, { timestamp: timestampUs, duration: FRAME_US });
    },
  };
}

// ---------- video encode run ----------

export async function encodeVideoRun(opts) {
  const {
    label,
    codec = 'avc1.4d0028',
    width = 1920,
    height = 1080,
    bitrate = 6_000_000,
    hardwareAcceleration = 'no-preference',
    frames = 600,
    paced = true,
    keyInterval = 60,
    latencyMode = 'realtime',
    bitrateMode = 'constant',
    timestampOriginUs = 0,
    source = 'canvas2d',
    maxQueue = 8,
  } = opts;
  const config = {
    codec,
    width,
    height,
    framerate: FPS,
    bitrate,
    hardwareAcceleration,
    latencyMode,
    bitrateMode,
    avc: { format: 'avc' },
  };
  const where = typeof WorkerGlobalScope !== 'undefined' ? 'worker' : 'main';
  const result = { label, where, hw: hardwareAcceleration, paced, codec, frames, source, latencyMode, maxQueue };
  let support;
  try {
    support = await VideoEncoder.isConfigSupported(config);
  } catch (e) {
    return { ...result, supported: false, error: String(e) };
  }
  result.supported = support.supported;
  result.config = support.config;
  if (!support.supported) return result;

  const pending = new Map();
  const latencies = [];
  const drawTimes = [];
  const keySizes = [];
  const pSizes = [];
  const inputTimestamps = new Set();
  const errors = [];
  let framesOut = 0;
  let bytes = 0;
  let tsMismatches = 0;
  let decoderConfigCount = 0;
  let description = null;
  let firstChunkSeen = false;
  let firstOutputTs = null;
  let queueMax = 0;
  let queueOver2 = 0;
  let outOfOrder = 0;
  let lastOutTs = -1;

  const encoder = new VideoEncoder({
    output: (chunk, meta) => {
      const now = performance.now();
      const t0 = pending.get(chunk.timestamp);
      if (t0 !== undefined) {
        latencies.push(now - t0);
        pending.delete(chunk.timestamp);
      }
      if (!inputTimestamps.has(chunk.timestamp)) tsMismatches++;
      if (chunk.timestamp < lastOutTs) outOfOrder++;
      lastOutTs = chunk.timestamp;
      if (firstOutputTs === null) firstOutputTs = chunk.timestamp;
      framesOut++;
      bytes += chunk.byteLength;
      if (chunk.type === 'key') keySizes.push(chunk.byteLength);
      else pSizes.push(chunk.byteLength);
      if (meta?.decoderConfig) {
        decoderConfigCount++;
        const d = bytesOf(meta.decoderConfig.description);
        if (!description) {
          description = {
            arrivedWithFirstChunk: !firstChunkSeen,
            codec: meta.decoderConfig.codec,
            codedWidth: meta.decoderConfig.codedWidth,
            codedHeight: meta.decoderConfig.codedHeight,
            length: d ? d.byteLength : 0,
            head: hex(d, 12),
          };
        }
      }
      firstChunkSeen = true;
    },
    error: (e) => errors.push(String(e)),
  });
  encoder.configure(config);

  let src;
  try {
    src = makeFrameSource(width, height, source);
  } catch (e) {
    encoder.close();
    return { ...result, supported: true, error: `frame source: ${e}` };
  }
  const tStart = performance.now();
  const frameIntervalMs = 1000 / FPS;
  for (let i = 0; i < frames; i++) {
    if (paced) {
      const wait = tStart + i * frameIntervalMs - performance.now();
      if (wait > 0) await sleep(wait);
    } else if (encoder.encodeQueueSize >= maxQueue) {
      await new Promise((r) => encoder.addEventListener('dequeue', r, { once: true }));
    }
    const d0 = performance.now();
    const frame = src.draw(i, timestampOriginUs + Math.round((i * 1e6) / FPS));
    drawTimes.push(performance.now() - d0);
    inputTimestamps.add(frame.timestamp);
    pending.set(frame.timestamp, performance.now());
    try {
      encoder.encode(frame, { keyFrame: i % keyInterval === 0 });
    } catch (e) {
      errors.push(String(e));
      frame.close();
      break;
    }
    frame.close();
    const q = encoder.encodeQueueSize;
    if (q > queueMax) queueMax = q;
    if (q > 2) queueOver2++;
  }
  const tLoopEnd = performance.now();
  try {
    await encoder.flush();
  } catch (e) {
    errors.push(String(e));
  }
  const wallMs = performance.now() - tStart;
  encoder.close();

  const framesIn = inputTimestamps.size;
  return {
    ...result,
    framesIn,
    framesOut,
    dropped: framesIn - framesOut,
    wallMs: round(wallMs),
    loopMs: round(tLoopEnd - tStart),
    flushMs: round(wallMs - (tLoopEnd - tStart)),
    fps: round(framesOut / (wallMs / 1000)),
    drawMs: summarize(drawTimes),
    encodeLatencyMs: summarize(latencies),
    queueMax,
    queueOver2,
    bytes,
    kbps: round((bytes * 8) / (framesOut / FPS) / 1000),
    keyframes: { count: keySizes.length, ...summarize(keySizes) },
    pframes: { count: pSizes.length, ...summarize(pSizes) },
    description,
    decoderConfigCount,
    firstOutputTs,
    tsMismatches,
    outOfOrder,
    stillPending: pending.size,
    errors,
  };
}

// ---------- audio encode run ----------

export async function encodeAudioRun(opts) {
  const {
    label,
    codec = 'mp4a.40.2',
    format = 'aac',
    seconds = 20,
    sampleRate = 48000,
    channels = 2,
    bitrate = 128_000,
    frameSize = 1024,
    timestampOriginUs = 0,
    bitrateMode,
  } = opts;
  const config = { codec, sampleRate, numberOfChannels: channels, bitrate };
  if (codec.startsWith('mp4a')) config.aac = { format };
  if (bitrateMode) config.bitrateMode = bitrateMode;
  const result = { label, codec, format: config.aac?.format ?? null, seconds, frameSize, bitrateMode: bitrateMode ?? null };
  let support;
  try {
    support = await AudioEncoder.isConfigSupported(config);
  } catch (e) {
    return { ...result, supported: false, error: String(e) };
  }
  result.supported = support.supported;
  result.config = support.config;
  if (!support.supported) return result;

  const inputTimestamps = [];
  const outputTimestamps = [];
  const durations = new Map();
  const sizes = [];
  const errors = [];
  let bytes = 0;
  let description = null;
  let decoderConfigCount = 0;
  let firstChunkSeen = false;

  const encoder = new AudioEncoder({
    output: (chunk, meta) => {
      outputTimestamps.push(chunk.timestamp);
      durations.set(chunk.duration, (durations.get(chunk.duration) ?? 0) + 1);
      sizes.push(chunk.byteLength);
      bytes += chunk.byteLength;
      if (meta?.decoderConfig) {
        decoderConfigCount++;
        const d = bytesOf(meta.decoderConfig.description);
        if (!description) {
          description = {
            arrivedWithFirstChunk: !firstChunkSeen,
            codec: meta.decoderConfig.codec,
            sampleRate: meta.decoderConfig.sampleRate,
            numberOfChannels: meta.decoderConfig.numberOfChannels,
            length: d ? d.byteLength : 0,
            hex: hex(d, 32),
          };
        }
      }
      firstChunkSeen = true;
    },
    error: (e) => errors.push(String(e)),
  });
  encoder.configure(config);

  const totalFrames = seconds * sampleRate;
  const rng = mulberry32(7);
  const t0 = performance.now();
  for (let s = 0; s < totalFrames; s += frameSize) {
    const n = Math.min(frameSize, totalFrames - s);
    const data = new Float32Array(n * channels);
    for (let i = 0; i < n; i++) {
      // Speech-like mix: a few harmonics plus decorrelated noise per channel, so the
      // encoder cannot collapse the stream to a few bits as it would for a pure tone.
      const k = (s + i) / sampleRate;
      const tone = 0.3 * Math.sin(2 * Math.PI * 440 * k) + 0.15 * Math.sin(2 * Math.PI * 1320 * k) + 0.1 * Math.sin(2 * Math.PI * 3300 * k);
      for (let c = 0; c < channels; c++) data[c * n + i] = tone + 0.1 * (rng() * 2 - 1);
    }
    const ts = timestampOriginUs + Math.round((s * 1e6) / sampleRate);
    inputTimestamps.push(ts);
    const ad = new AudioData({
      format: 'f32-planar',
      sampleRate,
      numberOfFrames: n,
      numberOfChannels: channels,
      timestamp: ts,
      data,
    });
    encoder.encode(ad);
    ad.close();
  }
  try {
    await encoder.flush();
  } catch (e) {
    errors.push(String(e));
  }
  const wallMs = performance.now() - t0;
  encoder.close();

  const inputSet = new Set(inputTimestamps);
  const preserved = outputTimestamps.filter((t) => inputSet.has(t)).length;
  const last = outputTimestamps.at(-1);
  const lastDuration = [...durations.keys()].at(-1);
  const expectedEndUs = timestampOriginUs + seconds * 1e6;
  return {
    ...result,
    chunks: outputTimestamps.length,
    inputs: inputTimestamps.length,
    wallMs: round(wallMs),
    bytes,
    kbps: round((bytes * 8) / seconds / 1000),
    chunkBytes: summarize(sizes),
    durationsUs: Object.fromEntries(durations),
    description,
    decoderConfigCount,
    firstInputTs: inputTimestamps[0],
    firstOutputTs: outputTimestamps[0],
    firstOutputTimestamps: outputTimestamps.slice(0, 4),
    lastOutputTs: last,
    endDriftUs: last === undefined ? null : last + (lastDuration ?? 0) - expectedEndUs,
    outputTimestampsPreserved: preserved,
    monotonic: outputTimestamps.every((t, i) => i === 0 || t >= outputTimestamps[i - 1]),
    errors,
  };
}

// ---------- camera path (worker side) ----------

export async function cameraProcessorRun(track, opts = {}) {
  const { frames = 300, hardwareAcceleration = 'prefer-hardware' } = opts;
  const where = typeof WorkerGlobalScope !== 'undefined' ? 'worker' : 'main';
  const settings = track.getSettings?.() ?? null;
  const processor = new MediaStreamTrackProcessor({ track });
  const reader = processor.readable.getReader();
  const pending = new Map();
  const latencies = [];
  const readGaps = [];
  const errors = [];
  let framesOut = 0;
  let bytes = 0;
  let encoder = null;
  let frameSize = null;
  let n = 0;
  let lastRead = null;
  const t0 = performance.now();
  try {
    while (n < frames) {
      const { value: frame, done } = await reader.read();
      if (done) break;
      const now = performance.now();
      if (lastRead !== null) readGaps.push(now - lastRead);
      lastRead = now;
      if (!encoder) {
        frameSize = { width: frame.codedWidth, height: frame.codedHeight };
        encoder = new VideoEncoder({
          output: (chunk) => {
            const t = pending.get(chunk.timestamp);
            if (t !== undefined) {
              latencies.push(performance.now() - t);
              pending.delete(chunk.timestamp);
            }
            framesOut++;
            bytes += chunk.byteLength;
          },
          error: (e) => errors.push(String(e)),
        });
        encoder.configure({
          codec: 'avc1.4d0028',
          width: frame.codedWidth,
          height: frame.codedHeight,
          framerate: FPS,
          bitrate: 6_000_000,
          hardwareAcceleration,
          latencyMode: 'realtime',
          avc: { format: 'avc' },
        });
      }
      pending.set(frame.timestamp, performance.now());
      encoder.encode(frame, { keyFrame: n % 60 === 0 });
      frame.close();
      n++;
    }
    if (encoder) await encoder.flush();
  } catch (e) {
    errors.push(String(e?.stack ?? e));
  }
  const wallMs = performance.now() - t0;
  try {
    reader.releaseLock();
  } catch {
    /* ignore */
  }
  try {
    track.stop();
  } catch {
    /* ignore */
  }
  encoder?.close();
  return {
    where,
    supported: true,
    settings,
    frameSize,
    frames: n,
    framesOut,
    wallMs: round(wallMs),
    fps: round(n / (wallMs / 1000)),
    readGapMs: summarize(readGaps),
    encodeLatencyMs: summarize(latencies),
    kbps: framesOut ? round((bytes * 8) / (framesOut / FPS) / 1000) : 0,
    errors,
  };
}

// Receives frames transferred from the main thread (fallback camera path).
export function makeFrameSink(opts = {}) {
  const { hardwareAcceleration = 'prefer-hardware' } = opts;
  const transferLatencies = [];
  const encodeLatencies = [];
  const pending = new Map();
  const errors = [];
  let encoder = null;
  let frames = 0;
  let framesOut = 0;
  let bytes = 0;
  let frameSize = null;
  let t0 = null;
  return {
    push(frame, sentAt) {
      const now = performance.now();
      if (t0 === null) t0 = now;
      transferLatencies.push(nowAbs() - sentAt);
      if (!encoder) {
        frameSize = { width: frame.codedWidth, height: frame.codedHeight };
        encoder = new VideoEncoder({
          output: (chunk) => {
            const t = pending.get(chunk.timestamp);
            if (t !== undefined) {
              encodeLatencies.push(performance.now() - t);
              pending.delete(chunk.timestamp);
            }
            framesOut++;
            bytes += chunk.byteLength;
          },
          error: (e) => errors.push(String(e)),
        });
        encoder.configure({
          codec: 'avc1.4d0028',
          width: frame.codedWidth,
          height: frame.codedHeight,
          framerate: FPS,
          bitrate: 6_000_000,
          hardwareAcceleration,
          latencyMode: 'realtime',
          avc: { format: 'avc' },
        });
      }
      pending.set(frame.timestamp, performance.now());
      try {
        encoder.encode(frame, { keyFrame: frames % 60 === 0 });
      } catch (e) {
        errors.push(String(e));
      }
      frame.close();
      frames++;
    },
    async stop() {
      try {
        if (encoder) await encoder.flush();
      } catch (e) {
        errors.push(String(e));
      }
      const wallMs = t0 === null ? 0 : performance.now() - t0;
      encoder?.close();
      return {
        supported: true,
        frameSize,
        frames,
        framesOut,
        wallMs: round(wallMs),
        fps: wallMs ? round(frames / (wallMs / 1000)) : 0,
        transferLatencyMs: summarize(transferLatencies),
        encodeLatencyMs: summarize(encodeLatencies),
        kbps: framesOut ? round((bytes * 8) / (framesOut / FPS) / 1000) : 0,
        errors,
      };
    },
  };
}
