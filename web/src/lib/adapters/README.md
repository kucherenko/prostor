# adapters

Thin TypeScript wrappers around browser APIs. Adapters execute what the Rust core and
the ADRs decide; the one piece of policy that lives here, the ingest backpressure of
ADR 0003 §4, is a pure module with unit tests.

- `media.ts` (main thread): `getUserMedia` (device ids, fallbacks, error wording), the Web
  Audio graph, and `MediaStreamTrackProcessor` pumps that hand `VideoFrame`s and
  `AudioData` to a sink (the compositor Worker). `webcodecs.d.ts` declares the
  Chrome-only processor type.
- `devices.ts` (main thread): the lobby's camera and microphone lists
  (`enumerateDevices`, refreshed on `devicechange`), the pure device picker, and the
  remembered choices (device ids, display name) in localStorage. Never the secret.
- `encoder.ts` (Worker): `VideoPipe` (H.264 Main 4.0, AVCC, realtime CBR, keyframe every
  60 frames, hardware → software fallback, queue cap 8, silent-drop accounting) and
  `AudioPipe` (AAC-LC; `null` where the platform has no AAC encoder).
- `ingest-ws.ts` (Worker): the ingest protocol client (`docs/protocol-ingest.md`): hello,
  `u32 seq` + tag framing, acks → lag, `end_of_stream`, reconnect; media only into a
  session that answered `started`, which always opens with the cached sequence headers.
- `backpressure.ts`: the lag policy (drop deltas until the next keyframe, keyframe
  request, bitrate step-down 25 % / slow step-up, degraded, reconnect thresholds).

Later: WebRTC (M5), signaling (M2/M5), storage (M3).
