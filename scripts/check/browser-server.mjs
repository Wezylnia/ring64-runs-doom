import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { extname, resolve, sep } from 'node:path';

export async function startBrowserServer(html, { isolated = false } = {}) {
  const root = resolve('.');
  const build = resolve(root, 'build') + sep;
  const server = createServer(async (request, response) => {
    try {
      const pathname = new URL(request.url ?? '/', 'http://localhost').pathname;
      const unisolatedPage = isolated && pathname === '/unisolated';
      if (isolated && !unisolatedPage) {
        response.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
        response.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
      }
      if (isolated) response.setHeader('Cache-Control', 'no-store');
      if (pathname === '/' || unisolatedPage) {
        response.writeHead(200, { 'content-type': 'text/html; charset=utf-8' });
        response.end(html);
        return;
      }
      const file = resolve(root, `.${pathname}`);
      if (!file.startsWith(build)) {
        response.writeHead(404).end();
        return;
      }
      const bytes = await readFile(file);
      response.writeHead(200, {
        'content-type': extname(file) === '.wasm' ? 'application/wasm' : 'text/javascript',
        'cache-control': 'no-store',
      });
      response.end(bytes);
    } catch {
      response.writeHead(404).end();
    }
  });

  await new Promise((ready, failed) => {
    server.once('error', failed);
    server.listen(0, '127.0.0.1', ready);
  });
  return {
    origin: `http://127.0.0.1:${server.address().port}`,
    close: () => new Promise((closed) => server.close(closed)),
  };
}
