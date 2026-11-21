import { execFileSync } from 'node:child_process';
import {
  copyFile,
  cp,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  rename,
  rm,
  writeFile,
} from 'node:fs/promises';
import { dirname, join, posix, resolve } from 'node:path';
import { build } from 'esbuild';
import hljs from 'highlight.js/lib/core';
import c from 'highlight.js/lib/languages/c';
import typescript from 'highlight.js/lib/languages/typescript';

const root = resolve(import.meta.dirname, '../..');
const source = join(root, 'website');
const generated = join(root, 'build');
const sdk = join(generated, 'npm/ring64');
const output = join(generated, 'website');

hljs.registerLanguage('c', c);
hljs.registerLanguage('typescript', typescript);

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
  const [masthead, canvas, controls] = await Promise.all([
    readFile(join(source, 'masthead.html'), 'utf8'),
    readFile(join(wetInk, 'canvas.html'), 'utf8'),
    readFile(join(wetInk, 'controls.html'), 'utf8'),
  ]);
  const exampleNames = ['add.ts', 'add.c', 'add-c.ts', 'parallel.ts'];
  const examples = await Promise.all(
    exampleNames.map(async (name) => {
      const code = await readFile(join(source, 'docs/examples', name), 'utf8');
      return hljs.highlight(code.trimEnd(), { language: name.endsWith('.c') ? 'c' : 'typescript' })
        .value;
    }),
  );
  for (const page of [
    'index.html',
    'under-the-ink/index.html',
    'docs/index.html',
    'docs/api/index.html',
  ]) {
    let html = await readFile(join(source, page), 'utf8');
    for (const [index, name] of exampleNames.entries()) {
      html = html.replace(`<!-- example:${name} -->`, () => examples[index]);
    }
    const destination = join(staged, page);
    const siteRoot = posix.relative(posix.dirname(page), '.') || '.';
    await mkdir(dirname(destination), { recursive: true });
    await writeFile(
      destination,
      html
        .replace('<!-- site-masthead -->', () =>
          masthead
            .replaceAll('{{root}}', siteRoot)
            .replaceAll('{{docsCurrent}}', String(page.startsWith('docs/')))
            .trim(),
        )
        .replace('<!-- wet-ink-canvas -->', () => canvas.trim())
        .replace('<!-- wet-ink-controls -->', () => controls.trim()),
    );
  }
  for (const entry of entries) {
    if (entry.isFile() && entry.name.endsWith('.css'))
      await copyFile(join(source, entry.name), join(staged, entry.name));
  }
  if (entries.some((entry) => entry.name === 'assets' && entry.isDirectory()))
    await cp(join(source, 'assets'), assets, { recursive: true });
  await mkdir(guestAssets, { recursive: true });

  await copyFile(join(root, 'assets/logo.svg'), join(assets, 'logo.svg'));
  await copyFile(join(wetInk, 'grain.png'), join(demo, 'grain.png'));
  await copyFile(join(wetInk, 'ink.c'), join(guestAssets, 'ink.c.txt'));
  for (const name of ['ring64.wasm', 'LICENSE', 'THIRD_PARTY_NOTICES.txt'])
    await copyFile(join(sdk, name), join(demo, name));
  await build({
    entryPoints: {
      site: join(source, 'site.ts'),
      docs: join(source, 'docs.ts'),
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
