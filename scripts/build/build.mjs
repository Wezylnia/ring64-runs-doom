import { execFileSync, spawnSync } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { copyFile, mkdir, readFile, rename, rm, writeFile } from 'node:fs/promises';
import { hostIdentity } from '../bench/emitted-host.mjs';

function run(command, args) {
  const result = spawnSync(command, args, { stdio: 'inherit' });
  if (result.error) throw result.error;
  if (result.status !== 0) process.exit(result.status ?? 1);
}

const inputs = [
  'src',
  'sdk/src',
  'scripts/build',
  '.cargo',
  'Cargo.toml',
  'Cargo.lock',
  'build.rs',
  'rust-toolchain.toml',
  'tsconfig.json',
  'package.json',
  'package-lock.json',
];
const git = (...args) =>
  execFileSync('git', args, { stdio: ['ignore', 'pipe', 'ignore'], maxBuffer: 32 * 1024 * 1024 });

async function sourceSnapshot() {
  let commit;
  try {
    commit = git('rev-parse', 'HEAD').toString().trim();
  } catch {
    return undefined;
  }
  const status = git('status', '--porcelain', '--untracked-files=all').toString().trimEnd();
  const fingerprint = createHash('sha256').update(
    git('diff', '--binary', '--no-ext-diff', '--no-textconv', commit, '--', ...inputs),
  );
  const untracked = git('ls-files', '--others', '--exclude-standard', '-z', '--', ...inputs);
  for (const path of untracked.toString().split('\0').filter(Boolean)) {
    fingerprint.update(path + '\0');
    fingerprint.update(
      createHash('sha256')
        .update(await readFile(path))
        .digest(),
    );
  }
  return {
    commit,
    workingTree: status ? status.split('\n') : [],
    inputsSha256: fingerprint.digest('hex'),
  };
}

const source = await sourceSnapshot();
const receipt = 'build/source.json';
await rm(receipt, { force: true });
run('cargo', ['build', '--locked', '--release', '--target', 'wasm32-unknown-unknown']);
await mkdir('build', { recursive: true });
await copyFile('target/wasm32-unknown-unknown/release/ring64.wasm', 'build/ring64.wasm');
run('node', ['scripts/build/profile-core.mjs', 'parallel']);
run('node', ['scripts/build/profile-core.mjs', 'memory64']);
run('node', ['scripts/build/build-sdk.mjs']);

if (source) {
  const coreSha256 = createHash('sha256')
    .update(await readFile('build/ring64.wasm'))
    .digest('hex');
  const { hostSha256 } = await hostIdentity('build/host');
  if (JSON.stringify(source) === JSON.stringify(await sourceSnapshot())) {
    const temporary = `${receipt}.${randomUUID()}`;
    try {
      await writeFile(
        temporary,
        JSON.stringify({ source, coreSha256, hostSha256 }, null, 2) + '\n',
      );
      await rename(temporary, receipt);
    } finally {
      await rm(temporary, { force: true });
    }
  }
}
