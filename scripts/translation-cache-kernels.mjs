const PC = 0x1000n;
const DATA = 0x10000n;
const DESCRIPTOR = 0x8000n;
const TABLES = 0x80000n;
const PAGE = 0x1000;
const RAM_BYTES = 0x60000;
const hex = (text) => Uint8Array.from(text.match(/../g), (byte) => parseInt(byte, 16));
const little = (value, width = 8) =>
  Uint8Array.from({ length: width }, (_, i) => Number((value >> BigInt(i * 8)) & 255n));
const equal = (actual, expected, label) => {
  if (actual !== expected) throw Error(`${label}: ${String(actual)} != ${String(expected)}`);
};
const bytesEqual = (actual, expected, label) => {
  equal(actual.length, expected.length, label + ' length');
  for (let i = 0; i < expected.length; i++) equal(actual[i], expected[i], `${label} byte${i}`);
};

const cells = [
  { name: 'hot/nonglobal-pcid0', kind: 'load', pages: 1, warm: true },
  { name: 'hot/global-pcid0', kind: 'load', pages: 1, warm: true, global: true },
  { name: 'hot/nonglobal-pcid1', kind: 'load', pages: 1, warm: true, pcid: 1 },
  { name: 'hot/global-pcid1', kind: 'load', pages: 1, warm: true, global: true, pcid: 1 },
  ...[1, 64, 65].flatMap((pages) =>
    [false, true].map((warm) => ({
      name: `working-set/${pages}-${warm ? 'warm' : 'cold'}`,
      kind: 'load',
      pages,
      warm,
    })),
  ),
  { name: 'working-set/63-warm-control', kind: 'load', pages: 63, warm: true },
  {
    name: 'store/first-d0-promotion',
    kind: 'store',
    pages: 1,
    warm: true,
    dirty: false,
    first: true,
  },
  { name: 'store/first-d1-control', kind: 'store', pages: 1, warm: true, dirty: true, first: true },
  { name: 'store/d1-steady', kind: 'store', pages: 1, warm: true, dirty: true },
  { name: 'churn/nop-control', kind: 'nop', pages: 1 },
  { name: 'churn/invlpg', kind: 'invlpg', pages: 1 },
  { name: 'churn/invpcid0', kind: 'invpcid', type: 0, pages: 1 },
  { name: 'churn/invpcid2', kind: 'invpcid', type: 2, pages: 1 },
];
export const translationCacheNames = cells.map((cell) => cell.name);

function loopCode(cell) {
  if (cell.kind === 'load') {
    // Five instructions/page plus four loop/reset instructions per complete sweep.
    return hex('488b134801d04881c30010000041ffc875ee4c89cb4189f8ffc975e4');
  }
  if (cell.kind === 'store') return hex('488903ffc975f9');
  if (cell.kind === 'nop') return hex('488b1390ffc975f8');
  if (cell.kind === 'invlpg') return hex('488b130f013bffc975f6');
  return hex('488b13660f388206ffc975f4');
}

export function translationCachePlan({
  iterations = 10001,
  samples = 11,
  seed = 1701,
  workloads = translationCacheNames,
} = {}) {
  for (const [name, value, max] of [
    ['iterations', iterations, 100001],
    ['samples', samples, 101],
    ['seed', seed, 0xffffffff],
  ])
    if (!Number.isSafeInteger(value) || value < 1 || value > max)
      throw Error(`${name} outside bounded integer range`);
  if (
    !workloads.length ||
    new Set(workloads).size !== workloads.length ||
    workloads.some((name) => !translationCacheNames.includes(name))
  )
    throw Error('unknown/duplicate cache workload');
  return cells
    .filter((cell) => workloads.includes(cell.name))
    .map((cell) => {
      const rounds = cell.first
        ? 1
        : cell.kind === 'load'
          ? Math.max(1, Math.ceil(iterations / cell.pages))
          : cell.kind === 'store'
            ? iterations
            : Math.min(iterations, 257);
      const perRound = cell.kind === 'load' ? 5 * cell.pages + 4 : cell.kind === 'store' ? 3 : 4;
      return {
        ...cell,
        global: cell.global ?? false,
        pcid: cell.pcid ?? 0,
        dirty: cell.dirty ?? true,
        warm: cell.warm ?? false,
        requestedIterations: iterations,
        rounds,
        samples,
        seed,
        code: Array.from(loopCode(cell)),
        retired: BigInt(rounds * perRound),
        payloadAccesses: BigInt(rounds * cell.pages),
        descriptorReads: BigInt(cell.kind === 'invpcid' ? rounds : 0),
        invalidations: BigInt(['invlpg', 'invpcid'].includes(cell.kind) ? rounds : 0),
        geometry:
          '64 total CPU translation entries; one hot code page. 63 data pages fit, 64/65 data pages pressure the cache. INVPCID additionally reads one descriptor page.',
        priming:
          cell.kind === 'store'
            ? 'Prime compiled loop with NOP, then one explicit guest data read; D0/D1 differs only in the initial leaf dirty bit.'
            : cell.warm
              ? 'Prime compiled loop with NOP, then one complete data sweep; scalar rearm preserves translation facts.'
              : 'Prime compiled loop with NOP only; first data sweep starts without retained data translations.',
        cap: cell.first
          ? 'First-promotion/control cells measure exactly one store plus DEC/JNZ; do not compare their total time against steady-loop totals.'
          : cell.kind === 'load'
            ? 'Round up target data accesses to complete page sweeps; report actual data accesses and retirements.'
            : cell.kind === 'store'
              ? 'Requested steady iterations.'
              : 'Invalidation/control cycles capped at257, including existing coarse code-owner fences.',
      };
    });
}

function initial(definition, rounds = definition.rounds) {
  const registers = Array(16).fill(0n);
  registers[0] = definition.kind === 'store' ? 0x1122334455667788n : BigInt(definition.type ?? 0);
  registers[1] = BigInt(rounds);
  registers[3] = DATA;
  if (definition.kind === 'load') {
    registers[7] = BigInt(definition.pages);
    registers[8] = BigInt(definition.pages);
    registers[9] = DATA;
  }
  if (definition.kind === 'invpcid') registers[6] = DESCRIPTOR;
  return {
    registers,
    flags: 2n,
    rip: PC + 1n,
    interruptibility: { sti: false, ss: false },
    event: { kind: 'ready' },
    interrupts: { irq: null, nmiPending: false, nmiBlocked: false },
  };
}

async function fixture(side, definition) {
  const ram = new Uint8Array(RAM_BYTES).fill(0xa5);
  ram.fill(0x82, Number(PC), Number(PC) + PAGE);
  ram[Number(PC)] = 0x90;
  ram.set(definition.code, Number(PC) + 1);
  ram.set(hex('488b13'), Number(PC) + 0x80);
  for (let page = 0; page < definition.pages; page++)
    ram.set(little(BigInt(page + 1)), Number(DATA) + page * PAGE);
  ram.set(little(BigInt(definition.pcid)), Number(DESCRIPTOR));
  ram.set(little(DATA), Number(DESCRIPTOR) + 8);
  const tables = new Uint8Array(0x4000),
    view = new DataView(tables.buffer);
  for (const [at, value] of [
    [0, TABLES + 0x1027n],
    [0x1000, TABLES + 0x2027n],
    [0x2000, TABLES + 0x3027n],
  ])
    view.setBigUint64(at, value, true);
  view.setBigUint64(0x3008, PC | 0x27n, true);
  view.setBigUint64(0x3040, DESCRIPTOR | 0x67n, true);
  for (let page = 0; page < definition.pages; page++) {
    const address = DATA + BigInt(page * PAGE);
    view.setBigUint64(
      0x3000 + Number(address >> 12n) * 8,
      address | 0x27n | (definition.dirty ? 0x40n : 0n) | (definition.global ? 0x100n : 0n),
      true,
    );
  }
  const permissions = { read: true, write: true, execute: true };
  const cpu = await side.createCpu({
    compiler: side.compiler,
    memoryModel: 'ia32e-paged',
    entry: PC,
    paging: {
      cr0: 0x80000033n,
      cr2: 0xdeadn,
      cr3: TABLES | BigInt(definition.pcid),
      cr4: 0x6a8n | (definition.pcid ? 1n << 17n : 0n),
      efer: 0x500n,
    },
    memory: {
      backings: [
        { id: 'ram', bytes: ram },
        { id: 'tables', bytes: tables },
      ],
      mappings: [
        { base: 0n, backing: 'ram', offset: 0, length: ram.length, permissions },
        {
          base: TABLES,
          backing: 'tables',
          offset: 0,
          length: tables.length,
          permissions: { ...permissions, execute: false },
        },
      ],
    },
    memoryLimits: { maxBackings: 2, maxMappings: 2, maxBackingBytes: ram.length + tables.length },
    limits: {
      maxInstructions: 32,
      maxSourceBytes: 256,
      maxArtifactBytes: 65536,
      maxCachedArtifacts: 8,
      maxCachedBytes: 1048576,
      maxTranslationsPerRun: 4 * definition.rounds + 16,
    },
  });
  try {
    const entry = initial(definition);
    cpu.setState({ ...entry, rip: PC });
    const codePrime = await cpu.run({ budget: 1n });
    equal(codePrime.reason, 'budget', definition.name + ' NOP priming');
    equal(codePrime.retired, 1n, definition.name + ' NOP retirement');
    equal(codePrime.state.rip, PC + 1n, definition.name + ' NOP next PC');
    let primingRetired = 1n;
    if (definition.kind === 'store') {
      cpu.setState({ ...initial(definition), rip: PC + 0x80n });
      const readPrime = await cpu.run({ budget: 1n });
      equal(readPrime.reason, 'budget', 'read priming reason');
      equal(readPrime.retired, 1n, 'read priming retirement');
      equal(readPrime.state.registers[2], 1n, 'read priming payload');
      primingRetired++;
    } else if (definition.warm) {
      cpu.setState(initial(definition, 1));
      const warm = await cpu.run({ budget: BigInt(5 * definition.pages + 4) });
      equal(warm.reason, 'budget', 'warm sweep reason');
      equal(warm.retired, BigInt(5 * definition.pages + 4), 'warm sweep retirements');
      equal(
        warm.state.registers[0],
        BigInt((definition.pages * (definition.pages + 1)) / 2),
        'warm sweep literal sum',
      );
      equal(warm.state.registers[2], BigInt(definition.pages), 'warm sweep last payload');
      equal(warm.state.rip, PC + 1n + BigInt(definition.code.length), 'warm sweep next PC');
      equal(warm.state.flags, 0x46n, 'warm sweep flags');
      primingRetired += warm.retired;
    }
    cpu.setState(initial(definition));
    return { cpu, primingRetired };
  } catch (error) {
    cpu.dispose();
    throw error;
  }
}

function verify(cpu, definition, result, beforeTime) {
  const label = definition.name;
  equal(result.reason, 'budget', label + ' reason');
  equal(result.retired, definition.retired, label + ' retirements');
  equal(result.remaining, 0n, label + ' remaining budget');
  equal(result.repeatIterations, 0n, label + ' repeat work');
  equal(result.state.rip, PC + 1n + BigInt(definition.code.length), label + ' next PC');
  equal(result.state.flags, 0x46n, label + ' DEC flags');
  const expected = initial(definition).registers;
  expected[1] = 0n;
  if (definition.kind === 'load') {
    expected[0] = BigInt((definition.rounds * definition.pages * (definition.pages + 1)) / 2);
    expected[2] = BigInt(definition.pages);
  } else if (definition.kind !== 'store') expected[2] = 1n;
  for (let i = 0; i < 16; i++)
    equal(result.state.registers[i], expected[i], label + ' raw GPR' + i);
  equal(result.state.event.kind, 'ready', label + ' event');
  if (
    result.fault ||
    result.pageFault ||
    result.physicalMemoryFault ||
    result.protectionFault ||
    result.alignmentCheck ||
    result.numericControlFault
  )
    throw Error(label + ' hidden fault');
  const time = cpu.readTimeState();
  equal(BigInt.asUintN(64, time.tsc - beforeTime.tsc), definition.retired, label + ' TSC work');
  equal(time.tscAux, beforeTime.tscAux, label + ' TSC_AUX');
  const bank = cpu.readPagingState();
  equal(bank.cr0, 0x80000033n, label + ' CR0');
  equal(bank.cr2, 0xdeadn, label + ' CR2');
  equal(bank.cr4, 0x6a8n | (definition.pcid ? 1n << 17n : 0n), label + ' CR4');
  equal(bank.efer, 0x500n, label + ' EFER');
  equal(bank.pdpte, undefined, label + ' no legacy quartet');
  equal(bank.cr3, TABLES | BigInt(definition.pcid), label + ' CR3');
  for (let page = 0; page < definition.pages; page++) {
    const address = DATA + BigInt(page * PAGE);
    const value = definition.kind === 'store' ? 0x1122334455667788n : BigInt(page + 1);
    bytesEqual(cpu.readPhysicalMemory(address, 8), little(value), label + ' payload');
    bytesEqual(
      cpu.readPhysicalMemory(address + BigInt(PAGE - 8), 8),
      new Uint8Array(8).fill(0xa5),
      label + ' page tail',
    );
    const leaf = cpu.readPhysicalMemory(TABLES + 0x3000n + (address >> 12n) * 8n, 8);
    const expectedLeaf = address | 0x67n | (definition.global ? 0x100n : 0n);
    bytesEqual(leaf, little(expectedLeaf), label + ' leaf A/D');
  }
  bytesEqual(
    cpu.readPhysicalMemory(PC + 1n, definition.code.length),
    Uint8Array.from(definition.code),
    label + ' unchanged code',
  );
  if (!definition.invalidations) {
    equal(result.resources.compilations, 0, label + ' already-compiled body');
    equal(result.resources.nativeEntries, 1, label + ' native entries');
    equal(result.resources.dispatches, 0, label + ' dispatches');
  }
  return { paging: bank, time, translations: cpu.inspectTranslations().length };
}

function shuffled(definitions, state) {
  const order = definitions.map((_, i) => i);
  for (let i = order.length - 1; i > 0; i--) {
    state.value ^= state.value << 13;
    state.value ^= state.value >>> 17;
    state.value ^= state.value << 5;
    const j = (state.value >>> 0) % (i + 1);
    [order[i], order[j]] = [order[j], order[i]];
  }
  return order;
}

export async function measureTranslationCaches(side, knobs) {
  const definitions = translationCachePlan(knobs),
    samples = [],
    schedule = [],
    state = { value: knobs.seed ?? 1701 };
  const count = knobs.validateOnly ? 1 : (knobs.samples ?? 11);
  for (let sample = 0; sample < count; sample++) {
    const order = shuffled(definitions, state);
    schedule.push(order.map((index) => definitions[index].name));
    for (const index of order) {
      const definition = definitions[index],
        { cpu, primingRetired } = await fixture(side, definition);
      try {
        const time = cpu.readTimeState();
        equal(time.tsc, primingRetired, definition.name + ' literal priming TSC');
        const firstLeafBytes = cpu.readPhysicalMemory(TABLES + 0x3080n, 8);
        const beforeFirstLeaf = new DataView(
          firstLeafBytes.buffer,
          firstLeafBytes.byteOffset,
          8,
        ).getBigUint64(0, true);
        equal(
          beforeFirstLeaf,
          DATA | 0x27n | (definition.dirty ? 0x40n : 0n) | (definition.global ? 0x100n : 0n),
          definition.name + ' pre-window first-leaf A/D',
        );
        const began = knobs.validateOnly ? undefined : performance.now();
        const result = await cpu.run({ budget: definition.retired });
        const elapsedMs = knobs.validateOnly ? undefined : performance.now() - began;
        const checked = verify(cpu, definition, result, time);
        samples.push({
          name: definition.name,
          sample,
          ...(elapsedMs === undefined ? {} : { elapsedMs }),
          retired: result.retired,
          payloadAccesses: definition.payloadAccesses,
          descriptorReads: definition.descriptorReads,
          invalidations: definition.invalidations,
          primingRetired,
          beforeFirstLeaf,
          resources: result.resources,
          ...(knobs.validateOnly ? {} : { timings: result.timings }),
          ...checked,
        });
      } finally {
        cpu.dispose();
      }
    }
  }
  return {
    kind: 'candidate-only-retained-translation-characterization',
    claimCeiling:
      'No ratios against Unsupported baseline; no hardware TLB counters. Payload/descriptor/invalidation operation counts come from literal loop work checked by full GPR/PC/retirement outcomes; public resource/A-D checks complement separate native walked-entry proofs.',
    schedulePolicy:
      'Seeded permutation per sample; each cell uses a fresh CPU, code priming and optional data priming outside elapsed run. No concurrent cells.',
    definitions,
    schedule,
    samples,
  };
}
