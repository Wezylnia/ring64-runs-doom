// Stages the browser build in build/web/:
//   ring64.js              the ring64 sdk bundled into one ES module
//   ring64*.wasm           the ring64 cores (memory32 and memory64)
//   doom-machine.js        the host side of the guest interface
//   doom.elf.txt           the guest program, gzip + base64
//   doom1.wad.txt          the shareware IWAD, gzip + base64
//   page.html              the page body (published as is)
//   index.html             the page wrapped in a document for local serving
//
// usage: node host/stage-web.mjs   (after `npm run build` in the repo root
//                                    and guest/build.sh)
import { build } from 'esbuild';
import { copyFile, mkdir, readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { gzipSync } from 'node:zlib';
import path from 'node:path';

const root = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const repo = path.resolve(root, '../..');
const sdk = path.join(repo, 'build/npm/ring64');
const out = path.join(root, 'build/web');
await mkdir(out, { recursive: true });

await build({
  entryPoints: [path.join(sdk, 'index.js')],
  bundle: true,
  format: 'esm',
  platform: 'browser',
  target: 'es2022',
  minify: true,
  outfile: path.join(out, 'ring64.js'),
  logLevel: 'warning',
});

await Promise.all([
  copyFile(path.join(sdk, 'ring64.wasm'), path.join(out, 'ring64.wasm')),
  copyFile(path.join(sdk, 'ring64-memory64.wasm'), path.join(out, 'ring64-memory64.wasm')),
  copyFile(path.join(root, 'host/doom-machine.js'), path.join(out, 'doom-machine.js')),
]);

// Artifacts serve text but not arbitrary binary types, so the guest program
// and the IWAD travel as base64 of their gzip stream.
for (const [source, target] of [
  ['build/doom.elf', 'doom.elf.txt'],
  ['build/doom1.wad', 'doom1.wad.txt'],
]) {
  const packed = gzipSync(await readFile(path.join(root, source)), { level: 9 });
  await writeFile(path.join(out, target), packed.toString('base64'));
}

const page = await readFile(path.join(root, 'web/index.html'), 'utf8');
await writeFile(path.join(out, 'page.html'), page);
await writeFile(
  path.join(out, 'index.html'),
  `<!doctype html>\n<html lang="en">\n<head>\n<meta charset="utf-8">\n` +
    `<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">\n` +
    `<style>body{margin:0}[hidden]{display:none!important}</style>\n</head>\n<body>\n${page}\n</body>\n</html>\n`,
);
console.log(`staged ${out}`);
