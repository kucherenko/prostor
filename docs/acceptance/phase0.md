# Phase 0 acceptance script (manual)

Status: **not yet passing** — the skeleton is under construction. Run this script after every
milestone in `docs/plan-phase0.md` and append a row to the results log.

## Preconditions

- Producer machine with Chromium (Chrome, Edge or Brave), a camera and a microphone.
- Guest machine on a different network (phone on mobile data is fine) with any modern browser.
- Twitch and YouTube test destinations: an RTMP URL and a stream key for each. Use an
  unlisted YouTube broadcast and a Twitch channel you own. Never paste keys into issues,
  logs or documents.
- Docker with Compose, `ffprobe` (ffmpeg) and `ffplay` or VLC on the producer machine.

## Steps

1. **Deploy.** `docker compose up` in the repo root. Open `http://localhost:8080`; the studio
   loads with no console errors.
2. **Unlock.** Open `/studio` and enter `PROSTOR_SECRET` (the value from
   `docker-compose.yml` or the environment).
3. **Scene 1.** Create a scene node on the board. Add a camera layer and a text layer
   reading `Scene 1`. The scene preview shows both layers.
4. **Scene 2.** Create a second scene with a camera layer and a text layer reading `Scene 2`.
   Draw a transition edge from scene 1 to scene 2 and set it to crossfade, 500 ms.
5. **Invite a guest.** Copy the join link (`/join/<room>?t=<token>`). Open it on the guest
   machine, allow camera and microphone. The producer sees the guest's tile with live video
   and audio level.
6. **Assign the guest.** Add a guest layer to scene 2 bound to the guest. Scene 2's preview
   shows the guest's video.
7. **Destinations.** Add the Twitch and YouTube destinations (URL + key). Keys are masked in
   the UI and absent from the exported board JSON.
8. **Go live.** Click Start. Within 10 seconds both destination status indicators read
   "live", the program preview shows scene 1, and the platform dashboards show an incoming
   stream.
9. **Switch.** Select scene 2 by click, then switch back and forth with the hotkey. The
   crossfade is visible in both platform players, audio stays in sync with video, and the
   guest's audio is audible only while scene 2 is live.
10. **Stop.** Click Stop. Both destinations go idle. `PROSTOR_RECORDINGS_DIR` (the compose
    volume) contains one `.flv` file for the session. `ffprobe` reports H.264 video,
    AAC audio and a duration close to the session length; the file plays in ffplay or VLC,
    including the crossfade.
11. **Measure.** Record in the results log:
    - glass-to-glass latency: show a running stopwatch to the camera, read the difference
      between the stopwatch and the platform player;
    - renderer fps, dropped frames and encoder queue depth from the studio status bar;
    - browser CPU from the Chromium task manager (renderer + GPU process) and server CPU
      from `docker stats`;
    - outgoing bitrate from the studio status bar and the platform's ingest health page.

## Pass criteria

All eleven steps complete without a restart; latency is measured (no target yet); fps holds
at 30 with fewer than 1 % dropped frames over five minutes; the recording plays.

## Results log

| Date | Commit | Result | Latency | fps / dropped | Browser CPU | Server CPU | Bitrate | Notes |
| ---- | ------ | ------ | ------- | ------------- | ----------- | ---------- | ------- | ----- |
