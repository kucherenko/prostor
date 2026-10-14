// Node client that sends as fast as the socket accepts, to find the server's
// receive ceiling. Uses the same record layout as sender.js.
//
//   node blast.mjs --url ws://127.0.0.1:9471/ws/ingest?id=blast --bytes 200000000 --frame 25000

import WebSocket from 'ws';

const argv = process.argv.slice(2);
const opt = (name, d) => {
  const i = argv.indexOf(name);
  return i >= 0 ? argv[i + 1] : d;
};
const url = opt('--url', 'ws://127.0.0.1:9471/ws/ingest?id=blast');
const total = Number(opt('--bytes', 200e6));
const frame = Number(opt('--frame', 25_000));
const inFlightLimit = Number(opt('--inflight', 1_000_000));

const buf = Buffer.alloc(frame);
buf.writeUInt32LE(frame - 4, 0);
const ws = new WebSocket(url, { perMessageDeflate: false });
let sent = 0;
let seq = 0;
let t0;

ws.on('open', () => {
  t0 = performance.now();
  const pump = () => {
    while (sent < total) {
      if (ws.bufferedAmount > inFlightLimit) {
        setImmediate(pump);
        return;
      }
      buf[4] = seq % 60 === 0 ? 1 : 2;
      buf.writeUInt32LE(seq, 8);
      buf.writeDoubleLE(Date.now(), 12);
      ws.send(buf, { binary: true });
      sent += frame;
      seq += 1;
    }
    ws.close(1000, 'done');
  };
  pump();
});

ws.on('close', () => {
  const ms = performance.now() - t0;
  console.log(JSON.stringify({ url, sentBytes: sent, frames: seq, ms: Math.round(ms), sendMbitPerSecond: +((sent * 8) / (ms / 1000) / 1e6).toFixed(0) }));
});

ws.on('error', (e) => {
  console.error(`error: ${e.message}`);
  process.exit(1);
});
