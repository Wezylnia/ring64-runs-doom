import { execFileSync } from 'node:child_process';
import { copyFile, cp, mkdir, mkdtemp, readdir, rename, rm } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { build } from 'esbuild';

const root = resolve(import.meta.dirname, '../..');
const source = join(root, 'website');
const generated = join(root, 'build');
const sdk = join(generated, 'npm/ring64');
const output = join(generated, 'website');
await mkdir(generated, { recursive: true });
const staged = await mkdtemp(join(generated, '.website-'));

try {
  const assets = join(staged, 'assets');
  const sdkAssets = join(assets, 'ring64');
  const guestAssets = join(assets, 'wet-ink');
  const wetInk = join(source, 'wet-ink');
  await mkdir(join(staged, 'wet-ink'), { recursive: true });

  const entries = await readdir(source, { withFileTypes: true });
  await copyFile(join(source, 'index.html'), join(staged, 'index.html'));
  for (const entry of entries) {
    if (entry.isFile() && /\.(css|mjs)$/.test(entry.name))
      await copyFile(join(source, entry.name), join(staged, entry.name));
  }
  if (entries.some((entry) => entry.name === 'assets' && entry.isDirectory()))
    await cp(join(source, 'assets'), assets, { recursive: true });
  await mkdir(sdkAssets, { recursive: true });
  await mkdir(guestAssets, { recursive: true });

  await copyFile(join(root, 'assets/logo.svg'), join(assets, 'logo.svg'));
  for (const name of ['index.mjs', 'worker.mjs', 'pigment.mjs', 'fullscreen.mjs', 'grain.png'])
    await copyFile(join(wetInk, name), join(staged, 'wet-ink', name));
  for (const name of ['ring64.wasm', 'LICENSE', 'THIRD_PARTY_NOTICES.txt'])
    await copyFile(join(sdk, name), join(sdkAssets, name));
  await build({
    entryPoints: [join(sdk, 'index.js')],
    outfile: join(sdkAssets, 'index.js'),
    bundle: true,
    minify: true,
    platform: 'browser',
    format: 'esm',
  });
  execFileSync('sh', [join(wetInk, 'build-guest.sh'), guestAssets], {
    cwd: wetInk,
    stdio: 'inherit',
  });

  await rm(output, { recursive: true, force: true });
  await rename(staged, output);
  console.log('Website built in build/website');
} finally {
  await rm(staged, { recursive: true, force: true });
}
