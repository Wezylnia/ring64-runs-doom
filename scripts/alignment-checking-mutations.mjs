import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import {
  cp,
  copyFile,
  lstat,
  mkdir,
  readFile,
  readdir,
  realpath,
  symlink,
  writeFile,
} from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { mutants, exclusions } from './alignment-checking-mutation-cases.mjs';

// Execution is opt-in. The source checkout is read-only; every edit and build uses
// a fresh /tmp copy. Run this driver serially with the repository's other gates.
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const arg = (key, fallback) => {
  const index = process.argv.indexOf(key);
  if (index < 0) return fallback;
  const value = process.argv[index + 1];
  if (!value || value.startsWith('--')) throw Error(`missing value for ${key}`);
  return value;
};
const hash = (bytes) => createHash('sha256').update(bytes).digest('hex');
const requested = arg('--only');
const names = requested && new Set(requested.split(',').map((name) => name.trim()));
const phase = arg('--phase', 'all');
if (!['all', 'native', 'public'].includes(phase))
  throw Error('phase must be all, native, or public');
if (names)
  for (const name of names)
    if (!mutants.some((m) => m.name === name)) throw Error(`unknown mutant: ${name}`);
const selected = mutants.filter(
  (m) => (!names || names.has(m.name)) && (phase === 'all' || m.kind === phase),
);
if (!selected.length) throw Error('empty mutation selection');
const originals = new Map();
for (const m of selected)
  for (const e of m.edits) {
    if (!originals.has(e.file)) originals.set(e.file, await readFile(join(root, e.file), 'utf8'));
    if (originals.get(e.file).split(e.old).length !== 2)
      throw Error(`${m.name}: anchor must match once in ${e.file}`);
    if (e.old === e.replacement) throw Error('a mutation must change production source');
  }
if (process.argv.includes('--check')) {
  console.log(
    JSON.stringify(
      {
        checkedAnchors: selected.reduce((n, m) => n + m.edits.length, 0),
        mutants: selected.length,
        executedProcesses: 0,
        exclusions,
      },
      null,
      2,
    ),
  );
  process.exit(0);
}
if (!process.argv.includes('--run') || process.argv.includes('--list')) {
  console.log(
    JSON.stringify(
      {
        usage:
          'node scripts/alignment-checking-mutations.mjs --run [--phase native|public] [--only name,...] [--work /tmp/new-dir] [--evidence /tmp/new-dir]',
        mutants: selected.map(({ name, kind, filter, edits }) => ({
          name,
          kind,
          filter,
          files: [...new Set(edits.map((e) => e.file))],
        })),
        exclusions,
      },
      null,
      2,
    ),
  );
  process.exit(0);
}

const stamp = new Date().toISOString().replaceAll(/[:.]/g, '-');
const work = resolve(arg('--work', `/tmp/ring64-alignment-mutations-${stamp}`));
const evidence = resolve(arg('--evidence', `/tmp/ring64-alignment-mutation-evidence-${stamp}`));
const temporaryRoot = await realpath('/tmp');
const repositoryRoot = await realpath(root);
for (const path of [work, evidence]) {
  if (
    !path.startsWith('/tmp/') ||
    (path === work && evidence.startsWith(work + '/')) ||
    (path === evidence && work.startsWith(evidence + '/'))
  )
    throw Error('separate fresh /tmp work and evidence directories are required');
  if (
    await lstat(path).catch((e) => {
      if (e.code !== 'ENOENT') throw e;
    })
  )
    throw Error(`directory already exists: ${path}`);
  const parent = await realpath(dirname(path));
  if (
    (parent !== temporaryRoot && !parent.startsWith(temporaryRoot + '/')) ||
    parent === repositoryRoot ||
    parent.startsWith(repositoryRoot + '/')
  )
    throw Error('temporary directory parent must not alias the checkout');
}
if (work === evidence) throw Error('work and evidence must differ');
await mkdir(work);
await mkdir(evidence);
for (const directory of ['src', 'host', 'tests'])
  await cp(join(root, directory), join(work, directory), { recursive: true });
for (const name of ['Cargo.toml', 'Cargo.lock', 'package.json', 'tsconfig.json'])
  await copyFile(join(root, name), join(work, name));
await symlink(await realpath(join(root, 'node_modules')), join(work, 'node_modules'), 'dir');
await mkdir(join(work, 'build'));
const driver = await readFile(fileURLToPath(import.meta.url));
const catalog = await readFile(
  fileURLToPath(new URL('./alignment-checking-mutation-cases.mjs', import.meta.url)),
);
await writeFile(join(evidence, 'alignment-checking-mutations.mjs'), driver);
await writeFile(join(evidence, 'alignment-checking-mutation-cases.mjs'), catalog);
const env = {
  ...process.env,
  CARGO_TARGET_DIR: join(await realpath(work), 'target'),
  RUST_BACKTRACE: '0',
};

async function identity(base) {
  const entries = [];
  async function visit(path) {
    for (const entry of (await readdir(join(base, path), { withFileTypes: true })).sort((a, b) =>
      a.name.localeCompare(b.name),
    )) {
      const name = join(path, entry.name);
      if (entry.isDirectory()) await visit(name);
      else if (entry.isFile()) entries.push([name, hash(await readFile(join(base, name)))]);
    }
  }
  for (const directory of ['src', 'host', 'tests']) await visit(directory);
  for (const name of ['Cargo.toml', 'Cargo.lock', 'package.json', 'tsconfig.json'])
    entries.push([name, hash(await readFile(join(base, name)))]);
  return { sha256: hash(JSON.stringify(entries)), files: entries };
}
const baselineIdentity = await identity(work);
await writeFile(join(evidence, 'source-identity.json'), JSON.stringify(baselineIdentity, null, 2));
const sourceCommit = spawnSync('git', ['rev-parse', 'HEAD'], {
  cwd: root,
  encoding: 'utf8',
}).stdout.trim();
const runner = `import { readFile } from 'node:fs/promises';
import { alignmentCheckingScenarios } from './tests/execution/alignment-checking/index.ts';
const compiler=await WebAssembly.compile(new Uint8Array(await readFile('build/ring64.wasm')));
const all=alignmentCheckingScenarios(compiler);
const selected=process.argv[2]?all.filter(s=>s.name.includes(process.argv[2])):all;
if(!selected.length)throw Error('empty scenario selection');
const passed=[];
for(const scenario of selected){
 try{await scenario.run();passed.push(scenario.name);}
 catch(error){
  const message=String(error.message),stack=String(error.stack);
  const assertions=['/tests/support/fixture.ts:','/tests/execution/alignment-checking/fixture.ts:','/tests/execution/supervisor-access/fixture.ts:'];
  const assertion=assertions.some(file=>stack.includes(file)) && !/cpu engine failed|WebAssembly|unreachable|out of bounds|compil|empty scenario|timed? ?out/i.test(message);
  console.log(JSON.stringify({selected:selected.length,passed:passed.length,failed:scenario.name,error:message,stack,assertion}));process.exitCode=1;break;
 }
}
if(!process.exitCode)console.log(JSON.stringify({selected:selected.length,passed:passed.length,names:passed}));
`;
await writeFile(join(work, 'mutation-cases.mjs'), runner);
await writeFile(join(evidence, 'mutation-cases.mjs'), runner);

async function run(label, command, args, timeout = 240000) {
  const result = spawnSync(command, args, {
    cwd: work,
    env,
    encoding: 'utf8',
    timeout,
    maxBuffer: 32 * 1024 * 1024,
  });
  const output = (result.stdout ?? '') + (result.stderr ?? '');
  const log = join(evidence, `${label}.log`);
  await writeFile(log, output);
  return {
    command: [command, ...args],
    status: result.status,
    signal: result.signal,
    error: result.error?.message,
    log,
    output,
  };
}
const publicResult = (result) => {
  const { output: _output, ...metadata } = result;
  return metadata;
};
async function build(label, kind) {
  const args =
    kind === 'native'
      ? ['test', '--locked', '--lib', '--no-run', '--message-format=json']
      : [
          'build',
          '--locked',
          '--release',
          '--target',
          'wasm32-unknown-unknown',
          '--message-format=json',
        ];
  const result = await run(`${label}-build`, 'cargo', args);
  if (result.status !== 0 || result.error || result.signal)
    throw Error(`${label}: compiler/setup failure (not a semantic kill); see ${result.log}`);
  const artifacts = result.output.split('\n').flatMap((line) => {
    try {
      const item = JSON.parse(line);
      return item.reason === 'compiler-artifact' && item.target.name === 'ring64' ? [item] : [];
    } catch {
      return [];
    }
  });
  const artifact =
    kind === 'native'
      ? artifacts.find((item) => item.executable && item.profile.test)
      : artifacts.find((item) => item.filenames?.some((name) => name.endsWith('/ring64.wasm')));
  if (!artifact) throw Error(`${label}: missing compiled artifact (not a semantic kill)`);
  const path =
    kind === 'native'
      ? artifact.executable
      : artifact.filenames.find((name) => name.endsWith('/ring64.wasm'));
  if (!path.startsWith(env.CARGO_TARGET_DIR + '/'))
    throw Error('compiled artifact escaped isolated target');
  const sha256 = hash(await readFile(path));
  if (kind === 'public') await copyFile(path, join(work, 'build/ring64.wasm'));
  return { path, sha256, fresh: artifact.fresh, process: publicResult(result) };
}
async function test(label, kind, filter, artifact) {
  const result =
    kind === 'native'
      ? await run(`${label}-test`, artifact.path, [filter, '--nocapture'])
      : await run(
          `${label}-test`,
          'node',
          ['--import', 'tsx', 'mutation-cases.mjs', filter],
          120000,
        );
  let detail,
    semantic = false;
  if (kind === 'native') {
    const passed = Number(result.output.match(/test result: ok\. (\d+) passed;/)?.[1] ?? 0);
    const failed = Number(
      result.output.match(/test result: FAILED\. \d+ passed; (\d+) failed;/)?.[1] ?? 0,
    );
    const names = [...result.output.matchAll(/^test (\S+) \.\.\. (?:ok|FAILED)$/gm)].map(
      (match) => match[1],
    );
    const panic = result.output.match(
      /thread '([^']+)'(?: \(\d+\))? panicked at (src\/[^:\n]+):(\d+):(\d+):\n([^\n]+)/,
    );
    let assertion;
    if (panic && panic[1].endsWith('::' + filter)) {
      const file = panic[2],
        line = Number(panic[3]);
      const original = await readFile(join(root, file), 'utf8');
      const copy = await readFile(join(work, file), 'utf8');
      const source = copy.split('\n')[line - 1];
      // A production panic/unwrap, compiler rejection, or modified oracle does not count.
      if (copy === original && /^\s*assert(?:_eq|_ne)?!\(/.test(source))
        assertion = {
          name: panic[1],
          file,
          line,
          source: source.trim(),
          oracleSha256: hash(copy),
          message: panic[5],
        };
    }
    detail = { passed, failed, names, assertion };
    semantic = result.status === 101 && failed === 1 && names.length === 1 && Boolean(assertion);
  } else {
    try {
      detail = JSON.parse(result.output.trim().split('\n').at(-1));
    } catch {
      /* infrastructure error */
    }
    semantic =
      result.status === 1 &&
      detail?.assertion === true &&
      detail.selected > 0 &&
      detail.failed.includes(filter);
  }
  if (result.error || result.signal) semantic = false;
  return { process: publicResult(result), detail, semantic };
}
async function restore() {
  for (const [file, content] of originals) await writeFile(join(work, file), content);
}
const baselineArtifacts = new Map(),
  baselineTests = [],
  restoredArtifacts = new Map(),
  restoredTests = [],
  results = [];
let fatal, finalIdentity, finalParentIdentity;
async function receipt() {
  const summary = {
    at: new Date().toISOString(),
    sourceCommit,
    driverSha256: hash(driver),
    catalogSha256: hash(catalog),
    work,
    evidence,
    target: env.CARGO_TARGET_DIR,
    sourceIdentitySha256: baselineIdentity.sha256,
    sourceIdentityScope:
      'src/** + host/** + tests/** + Cargo.toml/Cargo.lock/package.json/tsconfig.json',
    baselineArtifacts: Object.fromEntries(baselineArtifacts),
    baselineTests,
    restoredArtifacts: Object.fromEntries(restoredArtifacts),
    restoredTests,
    exclusions,
    results,
    restoredSourceMatches: finalIdentity?.sha256 === baselineIdentity.sha256,
    parentStillMatches: finalParentIdentity?.sha256 === baselineIdentity.sha256,
    experiments: results.length,
    semanticKills: results.filter((r) => r.status === 'semantic-kill').length,
    fatal,
  };
  await writeFile(join(evidence, 'results.json'), JSON.stringify(summary, null, 2));
  return summary;
}
try {
  // Every selected named oracle passes before any edit is admitted.
  for (const kind of new Set(selected.map((m) => m.kind))) {
    const artifact = await build(`baseline-${kind}`, kind);
    const snapshot = join(evidence, kind === 'native' ? 'baseline-native' : 'baseline.wasm');
    await copyFile(artifact.path, snapshot);
    baselineArtifacts.set(kind, { ...artifact, snapshot });
  }
  for (const [index, item] of [
    ...new Map(selected.map((m) => [m.kind + ':' + m.filter, m])).values(),
  ].entries()) {
    const checked = await test(
      `baseline-${index}`,
      item.kind,
      item.filter,
      baselineArtifacts.get(item.kind),
    );
    if (
      checked.process.status !== 0 ||
      checked.process.error ||
      checked.process.signal ||
      !(checked.detail?.passed > 0)
    )
      throw Error(`empty or failing unmutated oracle: ${item.filter}`);
    baselineTests.push({ kind: item.kind, filter: item.filter, ...checked });
  }
  await receipt();
  const seenArtifacts = new Set([...baselineArtifacts.values()].map((artifact) => artifact.sha256));
  const seenSources = new Set([baselineIdentity.sha256]);
  for (const mutant of selected) {
    let item = { name: mutant.name, kind: mutant.kind, filter: mutant.filter };
    const began = performance.now();
    try {
      await restore();
      const edits = [];
      for (const e of mutant.edits) {
        const source = await readFile(join(work, e.file), 'utf8');
        if (source.split(e.old).length !== 2) throw Error('nonunique anchor after restoration');
        edits.push({ ...e, line: source.slice(0, source.indexOf(e.old)).split('\n').length });
        await writeFile(join(work, e.file), source.replace(e.old, e.replacement));
      }
      item.edits = edits;
      await writeFile(join(evidence, `${mutant.name}-patch.json`), JSON.stringify(edits, null, 2));
      item.sourceSha256 = (await identity(work)).sha256;
      if (seenSources.has(item.sourceSha256))
        throw Error('source mutation does not produce a distinct candidate');
      seenSources.add(item.sourceSha256);
      const changesRust = mutant.edits.some((e) => e.file.startsWith('src/'));
      const artifact =
        mutant.kind === 'native' || changesRust
          ? await build(mutant.name, mutant.kind)
          : baselineArtifacts.get('public');
      item.artifact = artifact;
      item.artifactMutated = changesRust;
      if (changesRust && (artifact.fresh || seenArtifacts.has(artifact.sha256)))
        throw Error('mutant did not produce a distinct freshly compiled artifact');
      if (changesRust) seenArtifacts.add(artifact.sha256);
      const checked = await test(mutant.name, mutant.kind, mutant.filter, artifact);
      item = {
        ...item,
        ...checked,
        status: checked.semantic
          ? 'semantic-kill'
          : checked.process.status === 0 && checked.detail?.passed > 0
            ? 'survived'
            : 'uncounted-error',
      };
    } catch (error) {
      item.status = 'uncounted-error';
      item.error = error.message;
    } finally {
      try {
        await restore();
        item.restoredSourceMatches = (await identity(work)).sha256 === baselineIdentity.sha256;
        if (!item.restoredSourceMatches)
          item.restorationError = 'per-case source restoration failed';
        // A host-only mutant must never inherit the preceding mutated Wasm.
        const baselineWasm = baselineArtifacts.get('public');
        if (baselineWasm) await copyFile(baselineWasm.snapshot, join(work, 'build/ring64.wasm'));
      } catch (error) {
        item.restorationError = error.message;
      }
    }
    if (item.restorationError) item.status = 'uncounted-error';
    item.seconds = Math.round((performance.now() - began) / 10) / 100;
    results.push(item);
    await receipt();
    console.log(
      JSON.stringify({
        name: item.name,
        status: item.status,
        sourceSha256: item.sourceSha256,
        artifactSha256: item.artifact?.sha256,
        error: item.error,
        restorationError: item.restorationError,
        assertion: item.detail?.assertion ?? item.detail?.error,
        seconds: item.seconds,
      }),
    );
    if (item.restorationError) {
      fatal ??= item.restorationError;
      break;
    }
  }
} catch (error) {
  fatal = error.message;
} finally {
  try {
    await restore();
  } catch (error) {
    fatal ??= `final source restoration failed: ${error.message}`;
  }
}
try {
  finalIdentity = await identity(work);
} catch (error) {
  fatal ??= `final source identity failed: ${error.message}`;
}
// Validation follows cleanup so it cannot replace an earlier failure from finally.
if (finalIdentity?.sha256 === baselineIdentity.sha256)
  for (const kind of baselineArtifacts.keys()) {
    try {
      const restored = await build(`restored-${kind}`, kind);
      restoredArtifacts.set(kind, restored);
      if (restored.sha256 !== baselineArtifacts.get(kind).sha256)
        throw Error(`restored ${kind} artifact differs from baseline`);
      for (const [index, item] of [
        ...new Map(selected.filter((m) => m.kind === kind).map((m) => [m.filter, m])).values(),
      ].entries()) {
        const checked = await test(`restored-${kind}-${index}`, kind, item.filter, restored);
        restoredTests.push({ kind, filter: item.filter, ...checked });
        if (
          checked.process.status !== 0 ||
          checked.process.error ||
          checked.process.signal ||
          !(checked.detail?.passed > 0)
        )
          throw Error(`restored positive oracle failed: ${item.filter}`);
      }
    } catch (error) {
      fatal ??= error.message;
    }
  }
try {
  finalIdentity = await identity(work);
  finalParentIdentity = await identity(root);
} catch (error) {
  fatal ??= `final source identity failed: ${error.message}`;
}
const summary = await receipt();
console.log(
  JSON.stringify({
    evidence,
    experiments: summary.experiments,
    semanticKills: summary.semanticKills,
    restoredSourceMatches: summary.restoredSourceMatches,
    parentStillMatches: summary.parentStillMatches,
    fatal,
  }),
);
if (
  fatal ||
  results.some((item) => item.status !== 'semantic-kill') ||
  !summary.restoredSourceMatches ||
  !summary.parentStillMatches
)
  process.exitCode = 1;
