import { spawnSync } from 'node:child_process';
import { copyFile, mkdir } from 'node:fs/promises';

const target = 'wasm32-unknown-unknown';
const output = 'target/parallel';
const result = spawnSync(
  'cargo',
  [
    'rustc',
    '--locked',
    '--release',
    '--target',
    target,
    '--target-dir',
    output,
    '--lib',
    '--features',
    'parallel',
    '--',
    '-C',
    'link-arg=--import-memory',
    '-C',
    'link-arg=--export-memory',
    '-C',
    'link-arg=--initial-memory=2097152',
  ],
  { stdio: 'inherit' },
);
if (result.error) throw result.error;
if (result.status !== 0) process.exit(result.status ?? 1);
await mkdir('build', { recursive: true });
await copyFile(`${output}/${target}/release/ring64.wasm`, 'build/ring64-parallel.wasm');
