// prostor is a browser application: it needs WebCodecs, WebRTC and WASM at runtime,
// so nothing is rendered or prerendered on the server. The Rust server serves the
// static shell (`index.html`) for every route.
export const ssr = false;
export const prerender = false;
