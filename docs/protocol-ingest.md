# Ingest and signaling protocols v1

The producer's browser talks to the server over two WebSockets: the **signaling** socket
(`/ws/room/<room>`, text only) carries control, in Phase 0 the destinations and their keys;
the **ingest** socket (`/ws/ingest/<room>`) carries the encoded stream. Types live in
`prostor_model::{signaling, ingest}` (TypeScript mirrors in `web/src/lib/generated/`),
framing in `prostor_protocol::ingest`, the server side in `server/prostor-server/src/
{signaling,ingest}.rs`. Design: ADR 0003 (ingest, backpressure) and ADR 0004 (RTMP egress).

Both upgrades check the `Origin`: a browser must come from the same host and port the page
was served from, or from `PROSTOR_ALLOWED_ORIGINS` (comma-separated `scheme://host[:port]`)
when that is set; a request without an `Origin` (tools, tests) is allowed. Rejected: 403.

## Signaling: `/ws/room/<room>`

| Direction | `type` | Fields | Meaning |
| --- | --- | --- | --- |
| → server | `hello` | `auth: string \| null`, `role: producer \| guest` | First message, within 5 s. A producer's `auth` must equal `PROSTOR_SECRET` when one is configured. |
| → server | `start_stream` | `destinations: DestinationSpec[]` | Declares where the next ingest session goes (at most 8): `id`, `name`, `url` (`rtmp://` or `rtmps://` without the key; a query string is allowed and ends up on the key), `key`. Replaces any earlier declaration. Keys stay in memory for as long as this socket is open. |
| → server | `stop_stream` | – | Forgets the destinations and their keys. |
| ← browser | `welcome` | `room`, `role` | Answer to `hello`. |
| ← browser | `stream_ready` | `destinations: {id, name}[]` | Every destination parsed; the next ingest session fans out to them. |
| ← browser | `stream_stopped` | – | Answer to `stop_stream`. |
| ← browser | `error` | `code`, `message` | `invalid_destination` (names the id only, socket stays open, the earlier declaration stands); `unauthorized`, `bad_message`, `protocol` precede a close with 4401 / 4400 / 4400. |

Closing the producer's signaling socket forgets the destinations too. Guests are welcomed
and refused everything else until M5 adds WebRTC signaling.

## Ingest: `/ws/ingest/<room>`

## Endpoint and authentication

- `GET /ws/ingest/<room>` upgraded to a WebSocket. `<room>` is `[A-Za-z0-9_-]{1,64}`;
  anything else is a 400 before the upgrade. 503 when the socket cap (64) is reached or
  the server is shutting down.
- The first message must be a text `hello` within 5 s of connecting. When the server has
  `PROSTOR_SECRET`, `hello.auth` must equal it (compared in constant time); the value is
  never logged. Without a secret the server accepts any `hello`, which is only safe on the
  default loopback bind (the server warns otherwise).
- Messages are capped at 4 MiB. A peer that neither sends nor answers pings for 15 s is
  closed with 1008.

## Messages

Text frames carry JSON with a `type` field. Binary frames carry media.

| Direction | `type` | Fields | Meaning |
| --- | --- | --- | --- |
| → server | `hello` | `auth: string \| null`, `stream: StreamInfo` | Opens the session. `StreamInfo`: `width`, `height`, `fps`, `video_codec` (`avc1.*`), `video_bitrate`, and `audio_codec` (`mp4a.40.2` or `null`), `audio_bitrate`, `sample_rate`, `channels`. |
| → server | `end_of_stream` | `last_seq: u32` | No more media. The server drains its egresses, answers `ended` and closes. |
| → server | binary | `u32 BE seq` + one FLV tag | One complete FLV tag (11-byte header, body, 4-byte `PreviousTagSize`), exactly as the wasm `FlvMuxer` produces it. One tag per message. `seq` starts at 1 and must increase. |
| ← browser | `started` | `room`, `recording: string \| null` | The session is open; `recording` is the file name (never a path) being written. |
| ← browser | `ack` | `seq: u32`, `server_ms: f64` | Every live egress has consumed every tag up to and including `seq`. Sent every 100 ms when the value changed. `server_ms` is the server's Unix time in milliseconds. |
| ← browser | `egress_status` | `egress: EgressState[]` | Per egress: `id` (`file`, `rtmp-<destination id>`), `kind` (`file`, `rtmp`), `state` (`starting`, `live`, `degraded`, `reconnecting`, `failed`, `stopped`), `bytes`, `tags`, `dropped`, `last_seq`, `reconnects`, `detail` (the last problem in a few words, never a key). Sent every second and once more before `ended`. |
| ← browser | `ended` | `last_seq: u32` | Answer to `end_of_stream`, after the egresses finalized. Followed by close 1000. |
| ← browser | `error` | `code`, `message` | Sent just before a close with a 4xxx/1011 code. `message` never echoes client bytes. |

## Media rules

- The browser never sends the FLV file header or the `onMetaData` script tag; the server
  builds both from `StreamInfo` (script tags from the browser are refused).
- The AVC sequence header must arrive before the first media frame of either kind, and
  the AAC sequence header before the first audio frame; the two headers may come in either
  order (an AAC encoder usually answers first). Both are cached by the server and written
  in front of the first media frame; later sequence headers are forwarded in order.
- Audio tags in a stream with `audio_codec: null` are refused.
- Backpressure (ADR 0003 §4): the browser keeps the send time of every unacked `seq` and
  treats the age of the oldest unacked tag as its lag. Lag > 500 ms: drop non-key video
  until the next keyframe, request a keyframe, lower the bitrate 25 %; > 2 s: show
  "degraded"; > 10 s or no `ack` for 5 s: reconnect. Audio is never dropped.
- Server side, each egress has a queue of 192 tags and never makes the ingest socket wait:
  when the queue is full the tag is dropped for that egress and counted in `dropped`, and
  its video is skipped until the next keyframe (`state: degraded` while dropping). An RTMP
  egress that lost its connection drops everything while it reconnects with backoff
  (`state: reconnecting`), then restarts the stream with the sequence headers at timestamp
  0 and resumes video at a keyframe; a destination that refuses the key or the application
  ends with `state: failed`. Destinations must resolve to public addresses unless
  `PROSTOR_ALLOW_PRIVATE_DESTINATIONS=1`.

## Close codes (both sockets)

| Code | When | Preceded by |
| --- | --- | --- |
| 1000 | After `ended` | `egress_status`, `ended` |
| 1001 | Server shutdown; the recording is finalized first | – |
| 1008 | Idle peer | – |
| 1009 | Message over 4 MiB | – |
| 1011 | Recording could not be created, or an internal failure | `error{egress \| internal}` |
| 4400 | `hello` missing, late or repeated; a frame or message that does not parse; media out of order; unsupported codec or stream parameters; non-increasing `seq` | `error{bad_message \| protocol}` |
| 4401 | `hello.auth` missing or wrong when a secret is configured | `error{unauthorized}` |

The server completes every close handshake (it waits up to 2 s for the peer's close frame)
so the browser sees these codes rather than 1006.

## Example session

```text
signaling
→ {"type":"hello","auth":"…","role":"producer"}
← {"type":"welcome","room":"studio","role":"producer"}
→ {"type":"start_stream","destinations":[{"id":"d1","name":"Twitch","url":"rtmp://live.twitch.tv/app","key":"…"}]}
← {"type":"stream_ready","destinations":[{"id":"d1","name":"Twitch"}]}

ingest
→ {"type":"hello","auth":"…","stream":{"width":1920,"height":1080,"fps":30,"video_codec":"avc1.4d0028","video_bitrate":6000000,"audio_codec":"mp4a.40.2","audio_bitrate":128000,"sample_rate":48000,"channels":2}}
← {"type":"started","room":"studio","recording":"studio-20261014-181500.flv"}
← {"type":"egress_status","egress":[{"id":"file","kind":"file","state":"starting","bytes":0,"tags":0,"dropped":0,"last_seq":0,"reconnects":0,"detail":null},{"id":"rtmp-d1","kind":"rtmp","state":"starting","bytes":0,"tags":0,"dropped":0,"last_seq":0,"reconnects":0,"detail":null}]}
→ [00 00 00 01] + video tag (AVC sequence header)
→ [00 00 00 02] + audio tag (AAC sequence header)
→ [00 00 00 03] + video tag (keyframe)          # egresses start here
→ [00 00 00 04] + audio tag
…
← {"type":"ack","seq":4,"server_ms":1792001700123.4}
← {"type":"egress_status","egress":[{"id":"file","kind":"file","state":"live","bytes":48213,"tags":2,"dropped":0,"last_seq":4,"reconnects":0,"detail":null},{"id":"rtmp-d1","kind":"rtmp","state":"live","bytes":48213,"tags":2,"dropped":0,"last_seq":4,"reconnects":0,"detail":null}]}
…
→ {"type":"end_of_stream","last_seq":1842}
← {"type":"egress_status","egress":[{"id":"file","kind":"file","state":"stopped","bytes":7512033,"tags":1840,"dropped":0,"last_seq":1842,"reconnects":0,"detail":null},{"id":"rtmp-d1","kind":"rtmp","state":"stopped","bytes":7498120,"tags":1831,"dropped":9,"last_seq":1842,"reconnects":1,"detail":null}]}
← {"type":"ended","last_seq":1842}
← close 1000
```

`tags` is 1840, not 1842: the two sequence headers are cached and written as part of the
stream preamble rather than counted as consumed tags. The RTMP egress reconnected once
during the session and dropped the nine tags that arrived while it was down.

```text
signaling
→ {"type":"stop_stream"}
← {"type":"stream_stopped"}
```
