import { execFileSync } from 'node:child_process';
import { cp, rm } from 'node:fs/promises';

const emitted = 'build/sdk/src';
await Promise.all(
  ['build/sdk', 'build/tests', 'build/host'].map((path) =>
    rm(path, { recursive: true, force: true }),
  ),
);

execFileSync(process.execPath, ['node_modules/typescript/bin/tsc'], { stdio: 'inherit' });
execFileSync(process.execPath, ['scripts/generate-parallel-host.mjs'], { stdio: 'inherit' });
// Benchmarks use this artifact layout for both fresh and historical snapshots.
await cp(emitted, 'build/host', { recursive: true });
