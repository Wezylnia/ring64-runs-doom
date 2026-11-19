import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { cp, mkdir, mkdtemp, readFile, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { memoryMutations } from './parallel-mutate-memory.mjs';
import { protocolMutations } from './parallel-mutate-protocol.mjs';

const fetchScenario =
  'shared paged decode reaches byte 15 before #GP and repairs a peer-independent split fetch';
const mutations = [
  ...protocolMutations,
  ...memoryMutations,
  {
    name: 'partial-lane-reuses-stale-neighbor',
    path: 'src/memory/parallel.rs',
    from: 'let mut published = false;\n        for _ in 0..CAS_ATTEMPTS {\n            let next = current & !mask | value;',
    to: 'let mut published = false;\n        let stale = current;\n        for _ in 0..CAS_ATTEMPTS {\n            let next = stale & !mask | value;',
    check: [
      'cargo',
      'test',
      '--locked',
      'memory::parallel::tests::shared_partial_store_remerges_the_latest_neighbor_on_failed_cas',
      '--',
      '--exact',
    ],
    failure: /left: 52\s+right: 57140/,
    build: 'none',
  },
  {
    name: 'fetch-byte-sixteen',
    path: 'src/x86/decode/cursor.rs',
    from: 'if self.consumed == 15 {',
    to: 'if self.consumed == 16 {',
    check: [
      'env',
      `RING64_PARALLEL_SCENARIO=${fetchScenario}`,
      process.execPath,
      '--import',
      'tsx',
      '--test',
      'tests/runners/parallel-node.test.ts',
    ],
    failure: /required byte sixteen gives #GP: expected 13, received 14/,
    build: 'core',
  },
];

const names = process.argv.slice(2);
const cases = mutations.filter(({ name }) => !names.length || names.includes(name));
assert.equal(new Set(mutations.map(({ name }) => name)).size, mutations.length);
assert.ok(
  cases.length && names.every((name) => cases.some((entry) => entry.name === name)),
  `select from ${mutations.map(({ name }) => name).join(', ')}`,
);

const root = resolve('.');
const head = execFileSync('git', ['rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
const archive = execFileSync('git', ['archive', head], { maxBuffer: 64 * 1024 * 1024 });
const scratch = await mkdtemp(join(tmpdir(), 'ring64-mutations-'));
const tree = join(scratch, 'source');
const original = join(scratch, 'unmutated');
const receipts = resolve('build/verification/parallel-mutations');
const hash = (bytes) => createHash('sha256').update(bytes).digest('hex');
const caseDigest = hash(
  JSON.stringify(cases, (_, value) => (value instanceof RegExp ? value.toString() : value)),
);

function run(cwd, [command, ...args], options = {}) {
  const result = spawnSync(command, args, {
    cwd,
    env: { ...process.env, CARGO_TARGET_DIR: join(tree, 'target'), CARGO_TERM_COLOR: 'never' },
    encoding: 'utf8',
    timeout: 180_000,
    maxBuffer: 8 * 1024 * 1024,
    ...options,
  });
  if (result.error) throw result.error;
  if (result.signal) throw Error(`${command} terminated with ${result.signal}`);
  return { status: result.status, log: (result.stdout ?? '') + (result.stderr ?? '') };
}

function build(cmd) {
  const result = run(tree, [process.execPath, ...cmd]);
  assert.equal(result.status, 0, result.log);
}

async function check(out, entry, suffix, expectedFailure) {
  const result = run(tree, entry.check);
  await writeFile(join(out, `${entry.name}-${suffix}.log`), result.log);
  if (expectedFailure) {
    assert.notEqual(result.status, 0, `mutation survived: ${entry.name}`);
    assert.match(result.log, entry.failure, `${entry.name} failed without its expected witness`);
  } else {
    assert.equal(result.status, 0, `${entry.name} ${suffix}\n${result.log}`);
  }
  return result.status;
}

const core = 'ring64-parallel.wasm';
const rows = [];
try {
  await mkdir(receipts, { recursive: true });
  const out = await mkdtemp(join(receipts, `${head.slice(0, 7)}-`));
  console.log(`Mutation receipts: ${out}`);
  await mkdir(tree);
  const unpack = run(tree, ['tar', '-xf', '-'], { input: archive });
  assert.equal(unpack.status, 0, unpack.log);
  await symlink(join(root, 'node_modules'), join(tree, 'node_modules'), 'dir');
  build(['scripts/build.mjs']);
  await cp(join(tree, 'build'), original, { recursive: true });
  const baselineCore = hash(await readFile(join(original, core)));

  for (const entry of cases) {
    const path = join(tree, entry.path);
    const source = await readFile(path, 'utf8');
    assert.equal(source.split(entry.from).length, 2, `non-unique mutation anchor: ${entry.name}`);
    const baseline = await check(out, entry, 'baseline', false);
    await writeFile(path, source.replace(entry.from, entry.to));
    if (entry.build === 'host') {
      build(['node_modules/typescript/bin/tsc']);
      build(['scripts/generate-parallel-host.mjs']);
    } else if (entry.build === 'core') {
      build(['scripts/parallel-core.mjs']);
    }
    const mutatedCore = hash(await readFile(join(tree, 'build', core)));
    if (entry.build === 'core') assert.notEqual(mutatedCore, baselineCore);
    else assert.equal(mutatedCore, baselineCore);
    const mutant = await check(out, entry, 'mutant', true);

    await writeFile(path, source);
    if (entry.build === 'core') await cp(join(original, core), join(tree, 'build', core));
    if (entry.build === 'host')
      await cp(join(original, 'host'), join(tree, 'build', 'host'), { recursive: true });
    const restored = await check(out, entry, 'restored', false);
    rows.push({ name: entry.name, source: entry.path, baseline, mutant, restored, mutatedCore });
    await writeFile(
      join(out, 'summary.json'),
      JSON.stringify(
        { head, baselineCore, caseDigest, selected: cases.map(({ name }) => name), rows },
        null,
        2,
      ) + '\n',
    );
    console.log(`${entry.name}: baseline passed, mutation caught, restoration passed`);
  }
} finally {
  await rm(scratch, { recursive: true, force: true });
}
