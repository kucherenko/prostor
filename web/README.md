# prostor web

SvelteKit (Svelte 5 runes, TypeScript, Tailwind) single-page app served by the Rust
server. See the repository README for the workflow; the short version:

```sh
just dev        # from the repository root: builds the wasm package, runs server + Vite
pnpm test       # vitest (jsdom + @testing-library/svelte)
pnpm smoke      # headless Chrome against a running server (PROSTOR_URL)
```

Generated inputs, not committed: `src/lib/wasm/pkg` (`just build-wasm`).
Generated and committed: `src/lib/generated` (`just gen-types`).
