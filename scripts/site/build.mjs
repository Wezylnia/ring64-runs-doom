import { execFileSync } from 'node:child_process';
import { access, copyFile, cp, mkdir, mkdtemp, rename, rm } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { build } from 'esbuild';
import { writeSitePages } from './pages.mjs';

const root = resolve(import.meta.dirname, '../..');
const source = join(root, 'website');
const generated = join(root, 'build');
const sdk = join(generated, 'npm/ring64');
const output = join(generated, 'website');
const browserBundle = {
  alias: { ring64: join(sdk, 'index.js') },
  bundle: true,
  minify: true,
  platform: 'browser',
  target: 'es2022',
};

await access(join(sdk, 'ring64.wasm')).catch((error) => {
  if (error.code !== 'ENOENT') throw error;
  console.error('Ring64 SDK build is missing. Run npm run build first.');
  process.exit(1);
});

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

  const {
    outputFiles: [compiler],
  } = await build({
    ...browserBundle,
    entryPoints: [join(wetInk, 'compiler.ts')],
    alias: { ring64: join(sdk, 'compiler.js') },
    define: { 'import.meta.url': 'wetInkCompilerBase' },
    format: 'iife',
    write: false,
  });
  await writeSitePages(source, staged, compiler.text.trim());
  await copyFile(join(source, 'style.css'), join(staged, 'style.css'));
  await copyFile(join(source, 'docs/style.css'), join(staged, 'docs.css'));
  for (const file of ['style.css', 'results.json']) {
    await copyFile(join(source, 'benchmarks', file), join(staged, 'benchmarks', file));
  }
  await cp(join(source, 'assets'), assets, { recursive: true });
  await cp(join(source, 'docs/examples'), join(staged, 'docs/examples'), { recursive: true });
  await mkdir(guestAssets, { recursive: true });

  await copyFile(join(root, 'assets/logo.svg'), join(assets, 'logo.svg'));
  await copyFile(join(wetInk, 'grain.png'), join(demo, 'grain.png'));
  await copyFile(join(wetInk, 'guest/ink.c'), join(guestAssets, 'ink.c.txt'));
  for (const name of ['ring64.wasm', 'ring64-memory64.wasm', 'LICENSE', 'THIRD_PARTY_NOTICES.txt'])
    await copyFile(join(sdk, name), join(demo, name));
  await build({
    ...browserBundle,
    entryPoints: {
      site: join(source, 'site.ts'),
      docs: join(source, 'docs/navigation.ts'),
      'wet-ink/index': join(wetInk, 'index.ts'),
      'wet-ink/worker': join(wetInk, 'worker.ts'),
    },
    outdir: staged,
    format: 'esm',
  });
  execFileSync('sh', [join(wetInk, 'guest/build.sh'), guestAssets], {
    cwd: wetInk,
    stdio: 'inherit',
  });

  await rm(output, { recursive: true, force: true });
  await rename(staged, output);
  console.log('Website built in build/website');
} finally {
  await rm(staged, { recursive: true, force: true });
}
