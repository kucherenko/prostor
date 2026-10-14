// Page orchestration. Query parameters:
//   ?auto=1                 run on load (used by run.mjs)
//   ?phases=caps,video,audio,clock,camera   subset of phases (default: all)
//   ?frames=600 ?seconds=20 ?where=main,worker ?hw=hw,sw ?modes=paced,fast
// Results land in window.__results (JSON) and window.__done = true.
import { capabilityMatrix, encodeAudioRun, encodeVideoRun, envProbe, nowAbs, summarize, round } from './encode.js';

const params = new URLSearchParams(location.search);
const list = (key, def) => (params.get(key) ?? def).split(',').filter(Boolean);
const PHASES = list('phases', 'caps,video,audio,clock,camera');
const FRAMES = Number(params.get('frames') ?? 600);
const SECONDS = Number(params.get('seconds') ?? 20);
const WHERE = list('where', 'main,worker');
const HW = list('hw', 'hw,sw').map((h) => (h === 'hw' ? 'prefer-hardware' : 'prefer-software'));
const MODES = list('modes', 'paced,fast');
const SOURCES = list('sources', 'canvas2d');
const LATENCY = params.get('latency') ?? 'realtime';
const MAXQ = Number(params.get('maxq') ?? 8);

const $ = (id) => document.getElementById(id);
const results = { meta: {}, env: {}, caps: null, video: {}, audio: {}, clock: null, camera: null, errors: [] };
window.__results = results;
window.__done = false;

function status(text) {
  $('status').textContent = text;
  console.log(`[spike] ${text}`);
}

function phase(name, state) {
  globalThis.__phase?.(name, state);
  if (state === 'start') status(`running ${name}…`);
}

function render() {
  $('out').textContent = JSON.stringify(results, null, 2);
}

// ---- worker RPC ----
const worker = new Worker('./worker.js', { type: 'module' });
const waiting = new Map();
let nextId = 1;
worker.onmessage = (e) => {
  const { id, result } = e.data;
  const resolve = waiting.get(id);
  if (resolve) {
    waiting.delete(id);
    resolve(result);
  }
};
function callWorker(type, payload = {}, transfer = []) {
  const id = nextId++;
  return new Promise((resolve, reject) => {
    waiting.set(id, resolve);
    try {
      worker.postMessage({ id, type, ...payload }, transfer);
    } catch (e) {
      waiting.delete(id);
      reject(e);
    }
  });
}

async function guarded(name, fn) {
  phase(name, 'start');
  const t0 = performance.now();
  try {
    const r = await fn();
    phase(name, 'end');
    return r;
  } catch (e) {
    results.errors.push({ phase: name, error: String(e?.stack ?? e) });
    phase(name, 'end');
    return { error: String(e) };
  } finally {
    console.log(`[spike] ${name} took ${round(performance.now() - t0)} ms`);
    render();
  }
}

async function videoPhase() {
  for (const where of WHERE) {
    for (const hw of HW) {
      for (const mode of MODES) {
        for (const source of SOURCES) {
          const label =
            `${where}-${hw === 'prefer-hardware' ? 'hw' : 'sw'}-${mode}` +
            (source !== 'canvas2d' ? `-${source}` : '') +
            (LATENCY !== 'realtime' ? `-${LATENCY}` : '');
          const opts = {
            label,
            hardwareAcceleration: hw,
            paced: mode === 'paced',
            frames: FRAMES,
            source,
            latencyMode: LATENCY,
            maxQueue: MAXQ,
          };
          results.video[label] = await guarded(`video:${label}`, () =>
            where === 'worker' ? callWorker('video', { opts }) : encodeVideoRun(opts),
          );
        }
      }
    }
  }
}

async function audioPhase() {
  results.audio.aac = await guarded('audio:aac', () => encodeAudioRun({ label: 'aac', codec: 'mp4a.40.2', format: 'aac', seconds: SECONDS }));
  results.audio['aac-cbr'] = await guarded('audio:aac-cbr', () =>
    encodeAudioRun({ label: 'aac-cbr', codec: 'mp4a.40.2', format: 'aac', seconds: SECONDS, bitrateMode: 'constant' }),
  );
  results.audio.opus = await guarded('audio:opus', () => encodeAudioRun({ label: 'opus', codec: 'opus', seconds: SECONDS }));
}

async function clockPhase() {
  // One origin for both media clocks, in microseconds, far from zero.
  const originUs = Math.round(performance.now() * 1000) + 5_000_000;
  const video = await encodeVideoRun({
    label: 'clock-video',
    width: 640,
    height: 360,
    bitrate: 1_000_000,
    frames: 90,
    paced: false,
    timestampOriginUs: originUs,
  });
  const audio = await encodeAudioRun({ label: 'clock-audio', seconds: 2, timestampOriginUs: originUs });
  return {
    originUs,
    video: {
      supported: video.supported,
      framesIn: video.framesIn,
      framesOut: video.framesOut,
      firstOutputTs: video.firstOutputTs,
      tsMismatches: video.tsMismatches,
      preservedExactly: video.supported && video.tsMismatches === 0 && video.framesOut === video.framesIn,
    },
    audio: {
      supported: audio.supported,
      chunks: audio.chunks,
      firstInputTs: audio.firstInputTs,
      firstOutputTimestamps: audio.firstOutputTimestamps,
      preserved: audio.outputTimestampsPreserved,
      preservedExactly: audio.supported && audio.outputTimestampsPreserved === audio.chunks,
      endDriftUs: audio.endDriftUs,
      durationsUs: audio.durationsUs,
    },
  };
}

async function cameraPhase() {
  const constraints = { video: { width: 1920, height: 1080, frameRate: 30 } };
  const out = { trackTransfer: null, frameTransfer: null };

  // Path 1: transfer the MediaStreamTrack to the worker, process + encode there.
  const stream = await navigator.mediaDevices.getUserMedia(constraints);
  const track = stream.getVideoTracks()[0];
  out.sourceSettings = track.getSettings();
  try {
    out.trackTransfer = await callWorker('camera-track', { track, opts: { frames: 300 } }, [track]);
  } catch (e) {
    out.trackTransfer = { supported: false, error: String(e) };
    track.stop();
  }

  // Path 2: processor on the main thread, VideoFrames transferred to the worker.
  const stream2 = await navigator.mediaDevices.getUserMedia(constraints);
  const track2 = stream2.getVideoTracks()[0];
  const processor = new MediaStreamTrackProcessor({ track: track2 });
  const reader = processor.readable.getReader();
  await callWorker('frame-sink-start', { opts: {} });
  const postTimes = [];
  for (let i = 0; i < 300; i++) {
    const { value: frame, done } = await reader.read();
    if (done) break;
    const p0 = performance.now();
    worker.postMessage({ type: 'frame', frame, sentAt: nowAbs() }, [frame]);
    postTimes.push(performance.now() - p0);
  }
  reader.releaseLock();
  track2.stop();
  out.frameTransfer = await callWorker('frame-sink-stop');
  out.frameTransfer.postMessageMs = summarize(postTimes);
  return out;
}

async function run() {
  $('run').disabled = true;
  results.meta = {
    startedAt: new Date().toISOString(),
    href: location.href,
    phases: PHASES,
    frames: FRAMES,
    seconds: SECONDS,
  };
  results.env.window = envProbe();
  results.env.worker = await guarded('env:worker', () => callWorker('env'));
  if (PHASES.includes('caps')) results.caps = await guarded('caps', capabilityMatrix);
  if (PHASES.includes('video')) await videoPhase();
  if (PHASES.includes('audio')) await audioPhase();
  if (PHASES.includes('clock')) results.clock = await guarded('clock', clockPhase);
  if (PHASES.includes('camera')) results.camera = await guarded('camera', cameraPhase);
  results.meta.finishedAt = new Date().toISOString();
  render();
  status('done');
  window.__done = true;
  $('run').disabled = false;
}

$('run').addEventListener('click', run);
$('copy').addEventListener('click', () => navigator.clipboard.writeText(JSON.stringify(results, null, 2)));
if (params.get('auto') === '1') run();
