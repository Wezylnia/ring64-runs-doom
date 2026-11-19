import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdir, readFile, realpath, writeFile } from 'node:fs/promises';
import { relative, resolve } from 'node:path';
import { isDeepStrictEqual } from 'node:util';
import { build } from 'esbuild';

const [a, aTwin, b, out, ...flags] = process.argv.slice(2);
if (!a || !aTwin || !b || !out || flags.length > 2) {
  throw Error(
    'usage: parallel-private-control.mjs BASELINE SAME-BUILD-TWIN CANDIDATE OUT [--verify-only|node|chromium] [PAIRS]',
  );
}
const root = resolve(out),
  runtime = flags[0],
  pairs = Number(flags[1] ?? 12);
if (![undefined, '--verify-only', 'node', 'chromium'].includes(runtime))
  throw Error('invalid runtime');
if (!Number.isSafeInteger(pairs) || pairs < 12) throw Error('require at least 12 process pairs');
if ((await realpath(a)) === (await realpath(aTwin)))
  throw Error('A/A needs distinct existing module paths');
const hash = (bytes) => createHash('sha256').update(bytes).digest('hex');
const workloads = [
  'scalar-add',
  'checked-scalar-load',
  'budget-one-resume',
  'x87-load-store-m80',
  'x87-load-store-m80-resume',
  'x87-add-dependent-pc64',
  'x87-sqrt-with-stack-reset',
  'x87-round-with-stack-reset',
  'x87-budget-one-resume',
  'stack-push-pop',
  'far-call-ret-64-resume',
  'rep-forward-copy',
  'numeric-legacy-roundtrip',
];

function wasmSections(bytes) {
  let at = 8;
  const sections = [];
  function uint() {
    let number = 0,
      scale = 1;
    for (let i = 0; i < 5 && at < bytes.length; i++) {
      const byte = bytes[at++];
      number += (byte & 0x7f) * scale;
      if (byte < 0x80) return number;
      scale *= 128;
    }
    throw Error('malformed section length in private Wasm');
  }
  while (at < bytes.length) {
    const id = bytes[at++];
    const length = uint();
    const start = at;
    const end = at + length;
    if (end > bytes.length) throw Error('truncated private Wasm section');
    let custom;
    if (id === 0) {
      const nameLength = uint();
      if (at + nameLength > end) throw Error('malformed custom section name');
      custom = bytes.subarray(at, at + nameLength).toString();
    }
    sections.push({
      id,
      start,
      ...(custom !== undefined && { custom }),
      length,
      sha256: hash(bytes.subarray(start, end)),
    });
    at = end;
  }
  return sections;
}

async function identity(snapshot) {
  const folder = resolve(snapshot, 'build/host');
  const { metafile } = await build({
    absWorkingDir: folder,
    entryPoints: ['index.js'],
    bundle: true,
    write: false,
    platform: 'browser',
    format: 'esm',
    metafile: true,
    logLevel: 'silent',
  });
  const files = {};
  for (const name of Object.keys(metafile.inputs).sort()) {
    const path = resolve(folder, name);
    if (relative(folder, path).startsWith('..')) {
      throw Error('private module leaves its audited host tree: ' + name);
    }
    files[name] = hash(await readFile(path));
  }
  const core = await readFile(resolve(snapshot, 'build/ring64.wasm'));
  return { core: hash(core), sections: wasmSections(core), files };
}

function dataImage(bytes, section) {
  let at = section.start;
  function uleb() {
    let result = 0,
      factor = 1;
    while (at < section.start + section.length && factor <= 0x10000000) {
      const value = bytes[at++];
      result += (value & 127) * factor;
      if (value < 128) return result;
      factor *= 128;
    }
    throw Error('invalid data section integer');
  }
  if (uleb() !== 1 || uleb() !== 0 || bytes[at++] !== 0x41) {
    throw Error('private diagnostic audit expects one active memory32 i32.const segment');
  }
  const base = uleb();
  if (bytes[at++] !== 0x0b) throw Error('invalid data offset terminator');
  const length = uleb();
  if (at + length !== section.start + section.length)
    throw Error('unexpected data section geometry');
  return { base, payload: bytes.subarray(at, at + length) };
}

async function readSource(snapshot, name) {
  try {
    return (await readFile(resolve(snapshot, name), 'utf8')).split('\n');
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
  }
  let manifest;
  try {
    manifest = JSON.parse(await readFile(resolve(snapshot, 'paging-source.json'), 'utf8'));
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
  }
  const source = manifest?.source;
  if (source?.workingTree?.length === 0 && /^[a-f0-9]{40}$/.test(source.commit)) {
    const result = spawnSync('git', ['show', `${source.commit}:${name}`], { encoding: 'utf8' });
    if (result.error || result.status !== 0)
      throw Error('unable to read recorded Rust source ' + name);
    return result.stdout.split('\n');
  }
  return (await readFile(resolve(manifest?.path ?? snapshot, name), 'utf8')).split('\n');
}

async function movedRustLocations(before, after) {
  const oldBytes = await readFile(resolve(a, 'build/ring64.wasm'));
  const newBytes = await readFile(resolve(b, 'build/ring64.wasm'));
  const old = dataImage(
    oldBytes,
    before.sections.find((s) => s.id === 11),
  );
  const current = dataImage(
    newBytes,
    after.sections.find((s) => s.id === 11),
  );
  if (old.base !== current.base || old.payload.length !== current.payload.length) {
    throw Error('private initialized memory layout changed');
  }
  const changedWords = new Set();
  let changedBytes = 0;
  for (let offset = 0; offset < old.payload.length; offset++) {
    if (old.payload[offset] === current.payload[offset]) continue;
    changedBytes++;
    changedWords.add(offset - (offset % 4));
  }
  const records = [];
  const sources = new Map();
  for (const offset of changedWords) {
    if (offset < 8 || offset + 8 > old.payload.length) throw Error('invalid Rust location layout');
    const pointer = old.payload.readUInt32LE(offset - 8);
    const length = old.payload.readUInt32LE(offset - 4);
    const prior = old.payload.readUInt32LE(offset);
    const line = current.payload.readUInt32LE(offset);
    const column = old.payload.readUInt32LE(offset + 4);
    const pathStart = pointer - old.base;
    if (
      column === 0 ||
      current.payload.readUInt32LE(offset - 8) !== pointer ||
      current.payload.readUInt32LE(offset - 4) !== length ||
      current.payload.readUInt32LE(offset + 4) !== column ||
      pathStart < 0 ||
      length < 4 ||
      pathStart + length > old.payload.length
    ) {
      throw Error('private initialized data differs outside a Rust location line field');
    }
    const path = old.payload.subarray(pathStart, pathStart + length).toString();
    if (
      !/^src(?:\/[A-Za-z0-9_-]+)+\.rs$/.test(path) ||
      path !== current.payload.subarray(pathStart, pathStart + length).toString()
    ) {
      throw Error('private initialized data differs outside a Rust source location');
    }
    if (!sources.has(path))
      sources.set(path, await Promise.all([readSource(a, path), readSource(b, path)]));
    const [oldRows, newRows] = sources.get(path);
    const oldRow = oldRows[prior - 1],
      newRow = newRows[line - 1];
    if (
      !oldRow ||
      !newRow ||
      oldRow.trim() !== newRow.trim() ||
      column > oldRow.length + 1 ||
      column > newRow.length + 1
    ) {
      throw Error(
        `private location ${path}:${prior} -> ${line} does not point to the same Rust source`,
      );
    }
    records.push({ path, prior, line, column, offset });
  }
  return { changedBytes, changedWords: records.length, records };
}

const identities = await Promise.all([a, aTwin, b].map(identity));
await mkdir(root, { recursive: true });
const baseline = identities[0];
const candidate = identities[2];
const controlsEqual = isDeepStrictEqual(identities[1], baseline);
const structural = controlsEqual && isDeepStrictEqual(candidate, baseline);
const sameHostGraph = isDeepStrictEqual(candidate.files, baseline.files);
const sameExecutableSections = isDeepStrictEqual(
  baseline.sections.filter((s) => s.id !== 0 && s.id !== 11),
  candidate.sections.filter((s) => s.id !== 0 && s.id !== 11),
);
let diagnostics, validationError;
if (
  !structural &&
  controlsEqual &&
  sameHostGraph &&
  sameExecutableSections &&
  isDeepStrictEqual(
    baseline.sections.filter((s) => s.id === 0),
    candidate.sections.filter((s) => s.id === 0),
  )
) {
  try {
    diagnostics = await movedRustLocations(baseline, candidate);
  } catch (error) {
    validationError = String(error);
  }
}
const qualifiedForTiming = structural || diagnostics !== undefined;
await writeFile(
  resolve(root, 'identity.json'),
  JSON.stringify(
    {
      structural,
      sameHostGraph,
      sameExecutableSections,
      qualifiedForTiming,
      diagnostics,
      validationError,
      identities,
    },
    null,
    2,
  ) + '\n',
);
if (!qualifiedForTiming) throw Error('private change exceeds checked Rust diagnostic coordinates');
if (runtime === '--verify-only') process.exit(0);

function median(values) {
  const sorted = values.slice().sort((x, y) => x - y);
  return (sorted[(sorted.length - 1) >> 1] + sorted[sorted.length >> 1]) / 2;
}

// 1.8 rounds up t(.95, 11). It stays conservative for n >= 12 independent processes.
function interval(values) {
  const mean = values.reduce((sum, value) => sum + value, 0) / values.length;
  const variance =
    values.reduce((sum, value) => sum + (value - mean) ** 2, 0) / (values.length - 1);
  const half = 1.8 * Math.sqrt(variance / values.length);
  return {
    estimate: Math.expm1(mean),
    lower: Math.expm1(mean - half),
    upper: Math.expm1(mean + half),
  };
}

function compare(receipt) {
  if (receipt.unavailable.length) throw Error('private benchmark did not complete every workload');
  const joined = new Map();
  for (const sample of receipt.samples) {
    const key = sample.workload + '/' + sample.sample;
    const entry = joined.get(key) ?? {};
    entry[sample.side] = sample;
    joined.set(key, entry);
  }
  const grouped = new Map();
  for (const { baseline, current } of joined.values()) {
    if (!baseline || !current) throw Error('unmatched process observation');
    for (const name of ['state', 'context', 'x87', 'xmm', 'mxcsr', 'units', 'resources']) {
      if (!isDeepStrictEqual(baseline[name], current[name])) {
        throw Error(baseline.workload + '/' + name + ' is different guest work');
      }
    }
    const resume = baseline.wallMs !== undefined;
    const metrics = grouped.get(baseline.workload) ?? { warm: [], cold: [], first: [], native: [] };
    if (baseline.window === 'first') {
      metrics.cold.push(Math.log(current.creationMs / baseline.creationMs));
      metrics.first.push(Math.log(current.elapsedMs / baseline.elapsedMs));
    } else if (baseline.window === 'later') {
      const metric = resume ? 'wallMs' : 'elapsedMs';
      metrics.warm.push(Math.log(current[metric] / baseline[metric]));
      metrics.native.push(Math.log(current.elapsedMs / baseline.elapsedMs));
    }
    grouped.set(baseline.workload, metrics);
  }
  const output = Object.fromEntries(
    [...grouped].map(([name, m]) => [
      name,
      {
        warm: median(m.warm),
        cold: m.cold[0],
        first: m.first[0],
        native: median(m.native),
      },
    ]),
  );
  const setup = Object.fromEntries(receipt.setup.map(({ side, elapsedMs }) => [side, elapsedMs]));
  output['cold-import-and-core-compile'] = {
    cold: Math.log(setup.current / setup.baseline),
  };
  return output;
}

const summarized = [];
for (const engine of runtime ? [runtime] : ['node', 'chromium']) {
  const raw = { aa: [], ab: [] };
  for (let pair = 0; pair < pairs; pair++) {
    const backwards = (pair & 1) !== 0;
    const arms = backwards
      ? [
          ['ab', b],
          ['aa', aTwin],
        ]
      : [
          ['aa', aTwin],
          ['ab', b],
        ];
    for (const [arm, current] of arms) {
      const report = resolve(root, `${engine}-${pair}-${arm}.json`);
      const args = [
        resolve('scripts/benchmark.mjs'),
        '--baseline',
        resolve(a),
        '--candidate',
        resolve(current),
        '--cold-setup',
        '--workloads',
        workloads.join(','),
        '--iterations',
        '1000001',
        '--samples',
        '15',
        '--output',
        report,
        ...(engine === 'chromium' ? ['--browser'] : []),
        ...(backwards ? ['--reverse'] : []),
      ];
      const child = spawnSync(process.execPath, args, { encoding: 'utf8' });
      await writeFile(report.replace(/\.json$/, '.log'), child.stdout + child.stderr);
      if (child.error || child.status !== 0)
        throw child.error ?? Error('benchmark failed, see ' + report);
      raw[arm].push(compare(JSON.parse(await readFile(report, 'utf8'))));
    }
  }
  const rows = Object.entries(raw).flatMap(([arm, observations]) =>
    [...workloads, 'cold-import-and-core-compile'].flatMap((workload) =>
      (workload === 'cold-import-and-core-compile'
        ? ['cold']
        : ['warm', 'cold', 'first', 'native']
      ).map((metric) => {
        const data = observations.map((sample) => sample[workload]?.[metric]);
        if (data.some((value) => !Number.isFinite(value))) {
          return { arm, workload, metric, resolved: false, reason: 'timer resolution' };
        }
        const ci = interval(data);
        const bound = metric === 'warm' && !workload.endsWith('-resume') ? 0.01 : 0.03;
        const primary = metric === 'warm' || metric === 'cold';
        return {
          arm,
          workload,
          metric,
          ...ci,
          ...(primary && {
            bound,
            resolved: ci.lower >= -bound && ci.upper <= bound,
          }),
        };
      }),
    ),
  );
  summarized.push({ engine, pairs, rows });
  await writeFile(resolve(root, 'report.json'), JSON.stringify(summarized, null, 2) + '\n');
}
if (
  summarized.some(({ rows }) =>
    rows.some(({ metric, resolved }) => (metric === 'warm' || metric === 'cold') && !resolved),
  )
) {
  throw Error('private equivalence unresolved; inspect A/A and A/B process intervals in ' + root);
}
