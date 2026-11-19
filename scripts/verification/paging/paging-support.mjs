import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { cp, mkdir, readFile, symlink, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { hostIdentity } from '../../bench/emitted-host.mjs';

export const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../..');
export const baseline = '/tmp/ring64-paging-baseline-c1380a4';
export const encode = (value) =>
  JSON.stringify(value, (_, item) => (typeof item === 'bigint' ? item.toString() : item), 2) + '\n';
export const hash = (bytes) => createHash('sha256').update(bytes).digest('hex');

export function benchmarkScript(path) {
  const current = resolve(path, 'scripts/bench/benchmark.mjs');
  return existsSync(current) ? current : resolve(path, 'scripts/benchmark.mjs');
}

export async function manifest(path) {
  const core = await readFile(resolve(path, 'build/ring64.wasm'));
  const host = await hostIdentity(resolve(path, 'build/host'));
  const git = (args) =>
    execFileSync('git', ['-C', path, ...args], {
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
    }).trim();
  let source;
  try {
    source = JSON.parse(await readFile(resolve(path, 'paging-source.json'), 'utf8')).source;
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
    const changes = git(['status', '--porcelain']);
    source = {
      commit: git(['rev-parse', 'HEAD']),
      workingTree: changes ? changes.split('\n') : [],
    };
  }
  return {
    path,
    source,
    coreSha256: hash(core),
    coreBytes: core.length,
    ...host,
  };
}

// Freeze artifacts, not a source build: no Cargo invocation or shared target directory.
export async function freeze(path, name) {
  const before = await manifest(path);
  const target = resolve(root, 'build/verification', name);
  await mkdir(resolve(target, 'build'), { recursive: true });
  await cp(resolve(path, 'build/host'), resolve(target, 'build/host'), { recursive: true });
  await cp(resolve(path, 'build/ring64.wasm'), resolve(target, 'build/ring64.wasm'));
  await cp(resolve(root, 'benchmarks'), resolve(target, 'benchmarks'), { recursive: true });
  await mkdir(resolve(target, 'scripts/bench'), { recursive: true });
  await Promise.all(
    ['benchmark.mjs', 'emitted-host.mjs'].map((name) =>
      cp(resolve(root, 'scripts/bench', name), resolve(target, 'scripts/bench', name)),
    ),
  );
  await writeFile(resolve(target, 'package.json'), '{"type":"module"}\n');
  try {
    await symlink(
      resolve(root, 'node_modules'),
      resolve(target, 'node_modules'),
      process.platform === 'win32' ? 'junction' : 'dir',
    );
  } catch (error) {
    if (error.code !== 'EEXIST') throw error;
  }
  await writeFile(
    resolve(target, 'paging-source.json'),
    encode({ capturedAt: new Date().toISOString(), ...before }),
  );
  const after = await manifest(path),
    copy = await manifest(target);
  if (
    before.coreSha256 !== after.coreSha256 ||
    before.hostSha256 !== after.hostSha256 ||
    before.coreSha256 !== copy.coreSha256 ||
    before.hostSha256 !== copy.hostSha256
  )
    throw Error('build changed while freezing; discard snapshot and retry after build completes');
  return { ...copy, originalPath: path, source: before.source };
}
