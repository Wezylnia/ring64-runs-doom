// Serves build/web/ on http://localhost:8064 (or $PORT).
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const root = path.join(path.dirname(path.dirname(fileURLToPath(import.meta.url))), 'build/web');
const port = Number(process.env.PORT ?? 8064);
const types = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.wasm': 'application/wasm',
};

createServer(async (request, response) => {
  const url = new URL(request.url, 'http://localhost');
  const file = path.join(root, path.normalize(url.pathname === '/' ? '/index.html' : url.pathname));
  if (!file.startsWith(root)) return response.writeHead(403).end();
  try {
    const body = await readFile(file);
    response.writeHead(200, {
      'Content-Type': types[path.extname(file)] ?? 'application/octet-stream',
      'Cache-Control': 'no-cache',
    });
    response.end(body);
  } catch {
    console.log(`404 ${url.pathname}`);
    response.writeHead(404).end('not found');
  }
}).listen(port, () => console.log(`DOOM on ring64: http://localhost:${port}/`));
