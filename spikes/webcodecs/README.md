# Spike (a): WebCodecs H.264 + AAC

Measures encoder capability, 1080p30 throughput (main thread and Worker), AAC/Opus
behaviour, A/V clock preservation and the camera-to-Worker path. Results and conclusions:
`docs/spikes/0001-webcodecs-h264-aac.md`.

```sh
pnpm install
node run.mjs                     # headless Chrome, waits for an idle machine, writes results/*.json
node run.mjs --headed            # same, in a visible Chrome window
node run.mjs --no-wait --phases=caps,audio --frames=120 --sources=webgl2,canvas2d
node serve.mjs                   # then open http://127.0.0.1:9472/index.html in Chrome and press Run
```

On another OS: run `node serve.mjs`, open the page in Chrome, press Run, copy the JSON.
`CHROME_PATH` overrides the browser binary. Flags: `--gpu-flags`, `--features=`,
`--blink-features=`, `--where=main,worker`, `--hw=hw,sw`, `--modes=paced,fast`,
`--latency=quality`, `--maxq=8`, `--seconds=20`, `--label=name`.
