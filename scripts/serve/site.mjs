import { readFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { extname, resolve, sep } from 'node:path';

const root = resolve(import.meta.dirname, '../../build/website');
const host = '127.0.0.1';
const port = Number(process.env.RING64_SITE_PORT ?? 4173);
if (!Number.isInteger(port) || port < 1 || port > 65535)
  throw new Error('RING64_SITE_PORT must be an integer from 1 to 65535');
await readFile(resolve(root, 'index.html'));

const contentTypes = {
  '.bin': 'application/octet-stream',
  '.css': 'text/css; charset=utf-8',
  '.html': 'text/html; charset=utf-8',
  '.ico': 'image/x-icon',
  '.jpg': 'image/jpeg',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.png': 'image/png',
  '.svg': 'image/svg+xml',
  '.txt': 'text/plain; charset=utf-8',
  '.wasm': 'application/wasm',
  '.webp': 'image/webp',
  '.woff': 'font/woff',
  '.woff2': 'font/woff2',
};

const server = createServer(async (request, response) => {
  if (request.method !== 'GET' && request.method !== 'HEAD') {
    response.writeHead(405, { Allow: 'GET, HEAD' }).end();
    return;
  }

  let pathname;
  try {
    pathname = decodeURIComponent(new URL(request.url ?? '/', 'http://localhost').pathname);
  } catch {
    response.writeHead(400).end();
    return;
  }
  if (pathname.includes('\0')) {
    response.writeHead(400).end();
    return;
  }
  const file = resolve(root, `.${pathname.endsWith('/') ? pathname + 'index.html' : pathname}`);
  if (!file.startsWith(root + sep)) {
    response.writeHead(404).end();
    return;
  }

  try {
    const bytes = await readFile(file);
    response.writeHead(200, {
      'Cache-Control': 'no-store',
      'Content-Length': bytes.length,
      'Content-Type': contentTypes[extname(file)] ?? 'application/octet-stream',
    });
    response.end(request.method === 'HEAD' ? undefined : bytes);
  } catch (error) {
    if (['ENOENT', 'ENOTDIR', 'EISDIR'].includes(error.code)) response.writeHead(404).end();
    else {
      console.error(error);
      response.writeHead(500).end();
    }
  }
});

server.listen(port, host, () => console.log(`Website preview at http://${host}:${port}`));
