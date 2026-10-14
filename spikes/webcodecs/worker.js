// Worker side of the spike: the architecture encodes in a Worker, so every video
// measurement also runs here. Messages carry an `id`; replies echo it.
import { encodeVideoRun, envProbe, cameraProcessorRun, makeFrameSink } from './encode.js';

let sink = null;

function reply(id, result) {
  self.postMessage({ id, result });
}

self.onmessage = async (event) => {
  const { id, type } = event.data;
  try {
    switch (type) {
      case 'env':
        reply(id, envProbe());
        break;
      case 'video':
        reply(id, await encodeVideoRun(event.data.opts));
        break;
      case 'camera-track':
        reply(id, await cameraProcessorRun(event.data.track, event.data.opts));
        break;
      case 'frame-sink-start':
        sink = makeFrameSink(event.data.opts);
        reply(id, { ok: true });
        break;
      case 'frame':
        sink?.push(event.data.frame, event.data.sentAt);
        break;
      case 'frame-sink-stop': {
        const stats = sink ? await sink.stop() : { error: 'no sink' };
        sink = null;
        reply(id, stats);
        break;
      }
      default:
        reply(id, { error: `unknown message type ${type}` });
    }
  } catch (err) {
    reply(id, { error: String(err?.stack ?? err) });
  }
};
