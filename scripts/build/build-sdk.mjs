import { execFileSync } from 'node:child_process';
import { copyFile, cp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';

const emitted = 'build/sdk/src';
const packageRoot = 'build/npm/ring64';
await Promise.all(
  ['build/sdk', 'build/tests', 'build/host', packageRoot].map((path) =>
    rm(path, { recursive: true, force: true }),
  ),
);

execFileSync(process.execPath, ['node_modules/typescript/bin/tsc'], { stdio: 'inherit' });
for (const name of [
  'ring64.wasm',
  'ring64-memory64.wasm',
  'ring64-parallel.wasm',
  'ring64-parallel-memory64.wasm',
])
  await copyFile(`build/${name}`, `${emitted}/${name}`);

// Benchmarks use this artifact layout for both fresh and historical snapshots.
await cp(emitted, 'build/host', { recursive: true });
await mkdir('build/npm', { recursive: true });
await cp(emitted, packageRoot, { recursive: true });
const manifest = JSON.parse(await readFile('sdk/package.json', 'utf8'));
delete manifest.private;
await writeFile(`${packageRoot}/package.json`, JSON.stringify(manifest, null, 2) + '\n');
await copyFile('README.md', `${packageRoot}/README.md`);
await copyFile('LICENSE', `${packageRoot}/LICENSE`);
await copyFile('sdk/THIRD_PARTY_NOTICES.txt', `${packageRoot}/THIRD_PARTY_NOTICES.txt`);
