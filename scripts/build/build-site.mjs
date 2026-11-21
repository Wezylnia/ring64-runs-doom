import { execFileSync } from 'node:child_process';
import { copyFile, cp, mkdir, mkdtemp, readdir, rename, rm } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { build } from 'esbuild';

const root = resolve(import.meta.dirname, '../..');
const source = join(root, 'website');
const generated = join(root, 'build');
const sdk = join(generated, 'npm/ring64');
const output = join(generated, 'website');
execFileSync(
  process.execPath,
  [join(root, 'node_modules/typescript/bin/tsc'), '--project', join(source, 'tsconfig.json')],
  { cwd: root, stdio: 'inherit' },
);
await mkdir(generated, { recursive: true });
const staged = await mkdtemp(join(generated, '.website-'));

try {
  const assets = join(staged, 'assets');
  const guestAssets = join(assets, 'wet-ink');
  const wetInk = join(source, 'wet-ink');
  const demo = join(staged, 'wet-ink');
  await mkdir(demo, { recursive: true });

  const entries = await readdir(source, { withFileTypes: true });
  await copyFile(join(source, 'index.html'), join(staged, 'index.html'));
  for (const entry of entries) {
    if (entry.isFile() && entry.name.endsWith('.css'))
      await copyFile(join(source, entry.name), join(staged, entry.name));
  }
  if (entries.some((entry) => entry.name === 'assets' && entry.isDirectory()))
    await cp(join(source, 'assets'), assets, { recursive: true });
  await mkdir(guestAssets, { recursive: true });

  await copyFile(join(root, 'assets/logo.svg'), join(assets, 'logo.svg'));
  await copyFile(join(wetInk, 'grain.png'), join(demo, 'grain.png'));
  for (const name of ['ring64.wasm', 'LICENSE', 'THIRD_PARTY_NOTICES.txt'])
    await copyFile(join(sdk, name), join(demo, name));
  await build({
    entryPoints: {
      site: join(source, 'site.ts'),
      'wet-ink/index': join(wetInk, 'index.ts'),
      'wet-ink/worker': join(wetInk, 'worker.ts'),
    },
    outdir: staged,
    alias: { ring64: join(sdk, 'index.js') },
    bundle: true,
    minify: true,
    platform: 'browser',
    format: 'esm',
    target: 'es2022',
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
