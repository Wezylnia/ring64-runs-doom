import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFile, realpath } from 'node:fs/promises';
import { resolve } from 'node:path';

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
export async function sourceSnapshot(directory) {
  const git = (...args) =>
    execFileSync('git', args, {
      cwd: directory,
      stdio: ['ignore', 'pipe', 'ignore'],
      maxBuffer: 32 * 1024 * 1024,
    });
  let commit;
  try {
    const root = git('rev-parse', '--show-toplevel').toString().trimEnd();
    if ((await realpath(root)) !== (await realpath(directory))) return undefined;
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
        .update(await readFile(resolve(directory, path)))
        .digest(),
    );
  }
  return {
    commit,
    workingTree: status ? status.split('\n') : [],
    inputsSha256: fingerprint.digest('hex'),
  };
}
