// Tiny static server for the spike page. Workers and modules do not load from file://,
// so the page must be served over http. Usage: `node serve.mjs` (port 9472) or import
// `startServer()` from run.mjs.
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { dirname, extname, join, normalize } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = dirname(fileURLToPath(import.meta.url));
const TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
};

export function startServer(port = 9472) {
  const server = createServer(async (req, res) => {
    const url = new URL(req.url ?? '/', 'http://localhost');
    let path = normalize(url.pathname);
    if (path === '/' || path === '\\') path = '/index.html';
    const file = join(ROOT, path);
    if (!file.startsWith(ROOT)) {
      res.writeHead(403).end();
      return;
    }
    try {
      const body = await readFile(file);
      res.writeHead(200, {
        'content-type': TYPES[extname(file)] ?? 'application/octet-stream',
        'cache-control': 'no-store',
      });
      res.end(body);
    } catch {
      res.writeHead(404).end('not found');
    }
  });
  return new Promise((resolve) => {
    server.listen(port, '127.0.0.1', () => resolve(server));
  });
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const port = Number(process.env.PORT ?? 9472);
  await startServer(port);
  console.log(`spike page: http://127.0.0.1:${port}/index.html`);
}
