import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { cp, mkdir, readFile, rm, symlink, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { hostIdentity } from '../bench/emitted-host.mjs';

export const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
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

// compare the source and copy before publishing a frozen build.
export async function freeze(path, name) {
  const before = await manifest(path);
  const target = resolve(root, 'build/verification', name);
  await mkdir(dirname(target), { recursive: true });
  await mkdir(target);
  try {
    await mkdir(resolve(target, 'build'));
    await cp(resolve(path, 'build/host'), resolve(target, 'build/host'), { recursive: true });
    await cp(resolve(path, 'build/ring64.wasm'), resolve(target, 'build/ring64.wasm'));
    await cp(resolve(root, 'benchmarks'), resolve(target, 'benchmarks'), { recursive: true });
    for (const script of [
      'scripts/bench/benchmark.mjs',
      'scripts/bench/emitted-host.mjs',
      'scripts/support/browser.mjs',
    ]) {
      const destination = resolve(target, script);
      await mkdir(dirname(destination), { recursive: true });
      await cp(resolve(root, script), destination);
    }
    await writeFile(resolve(target, 'package.json'), '{"type":"module"}\n');
    await symlink(
      resolve(root, 'node_modules'),
      resolve(target, 'node_modules'),
      process.platform === 'win32' ? 'junction' : 'dir',
    );
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
      throw Error('build changed while freezing; retry after build completes');
    return { ...copy, originalPath: path, source: before.source };
  } catch (error) {
    await rm(target, { recursive: true, force: true });
    throw error;
  }
}
