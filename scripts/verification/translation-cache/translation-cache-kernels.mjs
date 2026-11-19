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
  for (let i = 0; i < expected.length; i++)
    if (actual[i] !== expected[i]) equal(actual[i], expected[i], `${label} byte${i}`);
};
const stable = (value) =>
  JSON.stringify(value, (_, item) =>
    typeof item === 'bigint'
      ? item.toString()
      : item && typeof item === 'object' && !Array.isArray(item)
        ? Object.fromEntries(Object.entries(item).sort(([a], [b]) => a.localeCompare(b)))
        : item,
  );
const same = (actual, expected, label) => equal(stable(actual), stable(expected), label);
const put = (bytes, at, value, width = 8) => bytes.set(little(value, width), Number(at));

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
  ...[
    ['large/2m-hot-load', 'large'],
    ['split/cross-4k-rmw', 'rmw'],
    ['writer/pte-code-invlpg', 'writer'],
    ['consumer/pg1-rep-copy', 'rep'],
    ['consumer/pg1-fx-roundtrip', 'fx'],
    ['consumer/pg1-int-iret', 'event'],
    ['switch/root-nonglobal', 'root'],
    ['switch/root-global', 'root-global'],
    ['switch/pcid-noflush', 'pcid-noflush'],
    ['switch/pcid-flush', 'pcid-flush'],
  ].map(([name, kind]) => ({
    name,
    kind,
    extended: true,
    commonIsa: !kind.startsWith('root') && !kind.startsWith('pcid'),
  })),
  {
    name: 'large/sparse-1g-hot-load',
    kind: 'large',
    extended: true,
    commonIsa: true,
    sparse: true,
  },
];
export const translationCacheNames = cells.map((cell) => cell.name);
export const translationCacheCommonNames = cells
  .filter((cell) => cell.commonIsa)
  .map((cell) => cell.name);

function extendedPlan(cell, iterations) {
  const bodies = {
    large: '488b03',
    rmw: '48830301',
    writer: '4889130f013fffd7488d34064881f200100000',
    fx: '480fae03660fefc0480fae0b',
    event: 'cd30',
    root: '410f22d8488b13488d0410410f22d9488b13488d0410',
  };
  const switching = cell.kind.startsWith('root') || cell.kind.startsWith('pcid');
  const rounds =
    ['writer', 'event'].includes(cell.kind) || switching ? Math.min(iterations, 257) : iterations;
  const body = hex(bodies[switching ? 'root' : cell.kind] ?? 'f3a4');
  const code = cell.kind === 'rep' ? body : Uint8Array.of(...body, 0xe2, 256 - body.length - 2);
  // Counts include the reached callee/handler, not merely bytes in the caller loop.
  const instructions = { large: 2, rmw: 2, writer: 8, rep: 1, fx: 4, event: 4 };
  const n = BigInt(rounds);
  const declaredWork = switching
    ? { dataReads: 2n * n, dataReadBytes: 16n * n }
    : {
        large: { dataReads: n, dataReadBytes: 8n * n },
        rmw: { rmwOperations: n, rmwReadBytes: 8n * n, rmwWriteBytes: 8n * n },
        writer: {
          sourcePteWrites: n,
          sourcePteWriteBytes: 8n * n,
          calls: n,
          returns: n,
          callStackWords: n,
          returnStackWords: n,
          stackWordBytes: 8,
        },
        rep: { copiedBytes: n, repByteReads: n, repByteWrites: n, retiredRepeatInstructions: 1 },
        fx: {
          imageReads: n,
          imageWrites: n,
          nominalImageReadBytes: 512n * n,
          nominalImageWriteBytes: 512n * n,
          selectedImageReadBytes: 367n * n,
          selectedImageWriteBytes: 367n * n,
        },
        event: {
          eventGateReads: n,
          eventGateBytes: 16n * n,
          frameWriteWords: 5n * n,
          frameReadWords: 5n * n,
          frameWordBytes: 8,
          eventDeliveries: n,
          iretInstructions: n,
        },
      }[cell.kind];
  return {
    ...cell,
    rounds,
    code: Array.from(code),
    retired: BigInt(cell.kind === 'rep' ? 1 : rounds * (switching ? 7 : instructions[cell.kind])),
    repeatWork: BigInt(cell.kind === 'rep' ? rounds : 0),
    declaredWork,
    declaredWorkScope:
      'Literal logical data/image/stack/gate/frame transfers completed by this fixture; excludes instruction fetch, additional GDT descriptor reads, helper calls, and page-table walks. Selected image bytes are architectural fields, not measured physical traffic. Event deliveries mean guest INT entries; hostPendingDelivery separately reports host pending-event dispatch attempts.',
    configuredMemory: {
      ramBytes: RAM_BYTES,
      tableBytes: 0x8000,
      maxBackingBytes: RAM_BYTES + 0x8000,
      maxBackings: 2,
      maxMappings: 2,
      maxCachedArtifacts: 16,
      maxCachedBytes: 1048576,
    },
    controlWrites: BigInt(switching ? 2 * rounds : 0),
    noFlushWrites: BigInt(cell.kind === 'pcid-noflush' ? 2 * rounds : 0),
    invalidations: BigInt(
      cell.kind === 'writer' ? rounds : switching && cell.kind !== 'pcid-noflush' ? 2 * rounds : 0,
    ),
    geometry:
      cell.kind === 'large'
        ? cell.sparse
          ? '4 KiB code at VA0x1000; a 1 GiB leaf at PDPTE1 maps sparse VA0x40010000 to literal GPA0x10000.'
          : 'One 2 MiB leaf covers code and data; literal physical addresses remain 0x1000/0x10000.'
        : '4 KiB leaves; bounded code/data/stack/table-alias working set below 64 entries.',
    priming:
      'One NOP primes code; hot large/root and split-RMW cells additionally perform one untimed literal data read. No host PTE edits in the measured interval.',
    cap:
      ['writer', 'event'].includes(cell.kind) || switching
        ? 'At most 257 complete control/source/event cycles; report exact completed guest work.'
        : 'Exact requested iterations/REP bytes.',
  };
}

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
      if (cell.extended)
        return {
          ...extendedPlan(cell, iterations),
          requestedIterations: iterations,
          samples,
          seed,
        };
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

function extendedState(definition) {
  const state = initial({ kind: 'extended', rounds: definition.rounds });
  state.registers[0] = 0n;
  state.registers[3] = definition.kind === 'rmw' ? 0x10ffcn : DATA;
  if (definition.sparse) state.registers[3] = 0x40010000n;
  if (definition.kind.startsWith('root') || definition.kind.startsWith('pcid')) {
    const pcid = definition.kind.startsWith('pcid');
    const hint = definition.kind === 'pcid-noflush' ? 1n << 63n : 0n;
    state.registers[8] = 0x84000n | (pcid ? 2n : 0n) | hint;
    state.registers[9] = TABLES | (pcid ? 1n : 0n) | hint;
  }
  if (definition.kind === 'writer') {
    state.registers[2] = 0x41067n;
    state.registers[3] = 0x20020n;
    state.registers[4] = 0x50000n;
    state.registers[7] = 0x4000n;
  }
  if (definition.kind === 'rep') {
    state.registers[6] = DATA;
    state.registers[7] = 0x30000n;
  }
  if (definition.kind === 'event') state.registers[4] = 0x50000n;
  return state;
}

function numericBank() {
  return {
    xmm: Array.from({ length: 16 }, (_, i) => (BigInt(i + 1) << 112n) | 0xfedcba9876543210n),
    mxcsr: 0x1f80,
    x87: {
      registers: Array.from({ length: 8 }, (_, i) => (BigInt(i + 1) << 64n) | 0x123456789abcdef0n),
      controlWord: 0x37f,
      statusWord: 0x2800,
      tagWord: 0xffff,
      instructionPointer: 0x1122334455667788n,
      dataPointer: 0x8877665544332211n,
      opcode: 0x123,
    },
  };
}

function fxImage(bank) {
  // Independent architectural field image; reserved bytes retain the literal A5 sentinel.
  const bytes = new Uint8Array(512).fill(0xa5);
  put(bytes, 0, BigInt(bank.x87.controlWord), 2);
  put(bytes, 2, BigInt(bank.x87.statusWord), 2);
  put(bytes, 4, 0n, 1);
  put(bytes, 6, BigInt(bank.x87.opcode), 2);
  put(bytes, 8, bank.x87.instructionPointer);
  put(bytes, 16, bank.x87.dataPointer);
  put(bytes, 24, BigInt(bank.mxcsr), 4);
  put(bytes, 28, 0xffffn, 4);
  for (let i = 0; i < 8; i++) put(bytes, 32 + 16 * i, bank.x87.registers[(5 + i) & 7], 10);
  for (let i = 0; i < 16; i++) put(bytes, 160 + 16 * i, bank.xmm[i], 16);
  return bytes;
}

async function extendedFixture(side, definition) {
  const ram = new Uint8Array(RAM_BYTES).fill(0xa5);
  ram.fill(0x82, Number(PC), Number(PC) + PAGE);
  ram[Number(PC)] = 0x90;
  ram.set(definition.code, Number(PC) + 1);
  ram.set(hex('488b13'), Number(PC) + 0x80);
  put(ram, DATA, 1n);
  put(ram, 0x20000, 7n);
  const tables = new Uint8Array(0x8000);
  const roots = definition.kind.startsWith('root') || definition.kind.startsWith('pcid');
  for (const offset of roots ? [0, 0x4000] : [0]) {
    const root = TABLES + BigInt(offset);
    put(tables, offset, root + 0x1027n);
    put(tables, offset + 0x1000, root + 0x2027n);
    put(tables, offset + 0x2000, root + 0x3027n);
    for (let page = 0; page < RAM_BYTES / PAGE; page++)
      put(tables, offset + 0x3000 + page * 8, BigInt(page * PAGE) | 0x67n);
    if (roots)
      put(
        tables,
        offset + 0x3080,
        (offset ? 0x20000n : DATA) | 0x67n | (definition.kind === 'root-global' ? 0x100n : 0n),
      );
  }
  if (definition.kind === 'large') put(tables, definition.sparse ? 0x1008 : 0x2000, 0xe7n);
  if (definition.kind === 'rmw') {
    put(ram, 0x10ffc, 0x10n);
    put(tables, 0x3080, 0x10027n);
    put(tables, 0x3088, 0x11027n);
  }
  if (definition.kind === 'writer') {
    ram.set(hex('b801000000c3'), 0x40000);
    ram.set(hex('b807000000c3'), 0x41000);
    put(tables, 0x3020, 0x40067n);
    put(tables, 0x3100, 0x83067n); // Guest 0x20020 writes the actual target leaf at PA 0x83020.
  }
  if (definition.kind === 'rep') {
    for (let i = 0; i < definition.rounds; i++) ram[Number(DATA) + i] = (i * 29 + 7) & 255;
    ram.fill(0xa5, 0x30000, 0x30000 + definition.rounds);
  }
  if (definition.kind === 'fx') ram.fill(0xa5, Number(DATA), Number(DATA) + 512);
  if (definition.kind === 'event') {
    ram.set(hex('48ffc248cf'), 0x4000);
    put(ram, 0x7008, 0x00af9b000000ffffn);
    put(ram, 0x7010, 0x00cf93000000ffffn);
    ram.fill(0, 0x9000, 0xa000);
    put(ram, 0x9300, 0x4000n, 2);
    put(ram, 0x9302, 8n, 2);
    ram[0x9305] = 0x8e; // A real IA32e sixteen-byte interrupt gate, no IST or privilege switch.
  }
  const bank = {
    cr0: 0x80000033n,
    cr2: 0xdeadn,
    cr3: TABLES | (definition.kind.startsWith('pcid') ? 1n : 0n),
    cr4:
      0x628n |
      (definition.kind === 'root-global' ? 0x80n : 0n) |
      (definition.kind.startsWith('pcid') ? 1n << 17n : 0n),
    efer: 0x500n,
  };
  const permissions = { read: true, write: true, execute: true };
  const cpu = await side.createCpu({
    compiler: side.compiler,
    memoryModel: 'ia32e-paged',
    entry: PC,
    paging: bank,
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
      maxCachedArtifacts: 16,
      maxCachedBytes: 1048576,
      maxTranslationsPerRun: 4 * definition.rounds + 16,
    },
  });
  try {
    const numeric = numericBank();
    cpu.setXmmState(numeric.xmm);
    cpu.setMxcsr(numeric.mxcsr);
    cpu.setX87State(numeric.x87);
    if (definition.kind === 'event') {
      const context = cpu.readContext();
      context.segments.cs.selector = 8;
      context.segments.ss.selector = 16;
      context.gdtr = { base: 0x7000n, limit: 0x17 };
      context.idtr = { base: 0x9000n, limit: 0xfff };
      cpu.setContext(context);
    }
    cpu.setState({ ...extendedState(definition), rip: PC });
    const prime = await cpu.run({ budget: 1n });
    equal(prime.reason, 'budget', definition.name + ' code priming');
    equal(prime.retired, 1n, definition.name + ' code prime retirement');
    let primingRetired = 1n;
    if (roots || ['large', 'rmw'].includes(definition.kind)) {
      cpu.setState({ ...extendedState(definition), rip: PC + 0x80n });
      const read = await cpu.run({ budget: 1n });
      equal(read.reason, 'budget', definition.name + ' data priming');
      equal(read.retired, 1n, definition.name + ' data prime retirement');
      equal(
        read.state.registers[2],
        definition.kind === 'rmw' ? 0x10n : 1n,
        definition.name + ' literal prime data',
      );
      primingRetired++;
    }
    cpu.setState(extendedState(definition));
    return {
      cpu,
      primingRetired,
      ram,
      tables,
      bank,
      numeric,
      context: cpu.readContext(),
      debug: cpu.readDebugState(),
    };
  } catch (error) {
    cpu.dispose();
    throw error;
  }
}

function verifyExtended(cpu, definition, result, beforeTime, prepared) {
  const label = definition.name;
  equal(result.reason, 'budget', label + ' reason');
  equal(result.retired, definition.retired, label + ' retirement');
  equal(result.remaining, 0n, label + ' budget');
  equal(result.repeatIterations, definition.repeatWork, label + ' repeat work');
  equal(
    result.remainingRepeatBudget,
    definition.kind === 'rep' ? 0n : definition.retired,
    label + ' remaining repeat budget',
  );
  for (const key of [
    'fault',
    'segmentFault',
    'alignmentFault',
    'alignmentCheck',
    'simdException',
    'generalProtectionFault',
    'x87Exception',
    'deviceNotAvailable',
    'numericControlFault',
    'protectionFault',
    'systemMemoryFault',
    'debugException',
    'pageFault',
    'physicalMemoryFault',
  ])
    equal(result[key], undefined, label + ' no ' + key);
  const expected = extendedState(definition);
  expected.rip = PC + 1n + BigInt(definition.code.length);
  expected.registers[1] = 0n;
  const ram = prepared.ram.slice(),
    tables = prepared.tables.slice();
  if (definition.kind === 'large') expected.registers[0] = 1n;
  if (definition.kind === 'rmw') {
    const value = 0x10n + BigInt(definition.rounds);
    put(ram, 0x10ffc, value);
    put(tables, 0x3080, 0x10067n);
    put(tables, 0x3088, 0x11067n);
    expected.flags =
      2n |
      (value % 16n === 0n ? 16n : 0n) |
      ([...Number(value & 255n).toString(2)].filter((bit) => bit === '1').length % 2 === 0
        ? 4n
        : 0n);
  }
  if (definition.kind.startsWith('root') || definition.kind.startsWith('pcid')) {
    expected.registers[0] = BigInt(definition.rounds * (definition.kind === 'root-global' ? 2 : 8));
    expected.registers[2] = 1n;
  }
  if (definition.kind === 'writer') {
    const odd = definition.rounds % 2;
    expected.registers[0] = odd ? 7n : 1n;
    expected.registers[2] = odd ? 0x40067n : 0x41067n;
    expected.registers[6] = BigInt(Math.floor(definition.rounds / 2) * 8 + odd * 7);
    put(tables, 0x3020, odd ? 0x41067n : 0x40067n);
    put(ram, 0x4fff8, PC + 9n);
  }
  if (definition.kind === 'rep') {
    ram.copyWithin(0x30000, Number(DATA), Number(DATA) + definition.rounds);
    expected.registers[6] += BigInt(definition.rounds);
    expected.registers[7] += BigInt(definition.rounds);
  }
  if (definition.kind === 'fx') ram.set(fxImage(prepared.numeric), Number(DATA));
  if (definition.kind === 'event') {
    // INT enters synchronously in the guest helper; no host pending-event dispatch is needed.
    equal(result.deliveryAttempts, 0n, label + ' no host pending-event delivery');
    equal(
      result.remainingDeliveryBudget,
      BigInt(definition.rounds),
      label + ' unused pending delivery budget',
    );
    expected.registers[2] = BigInt(definition.rounds);
    [PC + 3n, 8n, 2n, 0x50000n, 16n].forEach((word, i) => put(ram, 0x4ffd8 + i * 8, word));
  }
  same(result.state, expected, label + ' full architectural state');
  same(cpu.readPagingState(), prepared.bank, label + ' full paging bank');
  same(cpu.readContext(), prepared.context, label + ' complete context');
  same(cpu.readDebugState(), prepared.debug, label + ' debug state');
  same(cpu.readXmmState(), prepared.numeric.xmm, label + ' full XMM bank');
  equal(cpu.readMxcsr(), prepared.numeric.mxcsr, label + ' MXCSR');
  same(cpu.readX87State(), prepared.numeric.x87, label + ' full x87 bank');
  bytesEqual(
    cpu.readPhysicalMemory(0n, ram.length),
    ram,
    label + ' full payload/code/stack/guards',
  );
  bytesEqual(
    cpu.readPhysicalMemory(TABLES, tables.length),
    tables,
    label + ' full hierarchy A/D/leaf bytes',
  );
  const time = cpu.readTimeState();
  equal(BigInt.asUintN(64, time.tsc - beforeTime.tsc), definition.retired, label + ' exact TSC');
  equal(time.tscAux, beforeTime.tscAux, label + ' TSC_AUX');
  return {
    paging: cpu.readPagingState(),
    time,
    translations: cpu.inspectTranslations().length,
    resultState: result.state,
  };
}

async function fixture(side, definition) {
  if (definition.extended) return extendedFixture(side, definition);
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

function verify(cpu, definition, result, beforeTime, prepared) {
  if (definition.extended) return verifyExtended(cpu, definition, result, beforeTime, prepared);
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

const fingerprint = async (value) =>
  Array.from(
    new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(stable(value)))),
  )
    .map((byte) => byte.toString(16).padStart(2, '0'))
    .join('');

export async function measureTranslationCaches(side, knobs, baselineSide) {
  const definitions = translationCachePlan(knobs),
    samples = [],
    schedule = [],
    pairedSchedule = [],
    state = { value: knobs.seed ?? 1701 };
  if (baselineSide && definitions.some((definition) => !definition.commonIsa))
    throw Error('paired comparison accepts only common-ISA cells; new controls are candidate-only');
  const count = knobs.validateOnly ? 1 : (knobs.samples ?? 11);
  for (let sample = 0; sample < count; sample++) {
    const order = shuffled(definitions, state);
    schedule.push(order.map((index) => definitions[index].name));
    for (const index of order) {
      const definition = definitions[index];
      const sides = baselineSide
        ? [
            ['baseline', baselineSide],
            ['current', side],
          ]
        : [['current', side]];
      if (baselineSide && Boolean(sample % 2) !== Boolean(knobs.reverse)) sides.reverse();
      const paired = [];
      for (const [label, selectedSide] of sides) {
        pairedSchedule.push({ sample, name: definition.name, side: label });
        const prepared = await fixture(selectedSide, definition),
          { cpu, primingRetired } = prepared;
        try {
          const time = cpu.readTimeState();
          equal(time.tsc, primingRetired, definition.name + ' literal priming TSC');
          const firstLeafBytes = definition.extended
            ? undefined
            : cpu.readPhysicalMemory(TABLES + 0x3080n, 8);
          const beforeFirstLeaf = firstLeafBytes
            ? new DataView(firstLeafBytes.buffer, firstLeafBytes.byteOffset, 8).getBigUint64(
                0,
                true,
              )
            : undefined;
          if (!definition.extended)
            equal(
              beforeFirstLeaf,
              DATA | 0x27n | (definition.dirty ? 0x40n : 0n) | (definition.global ? 0x100n : 0n),
              definition.name + ' pre-window first-leaf A/D',
            );
          const ownersBefore = cpu.inspectTranslations().map((unit) => unit.owner);
          const began = knobs.validateOnly ? undefined : performance.now();
          const result = await cpu.run({
            budget: definition.retired,
            ...(definition.kind === 'rep' ? { repeatBudget: definition.repeatWork } : {}),
            ...(definition.kind === 'event'
              ? { eventDelivery: { budget: BigInt(definition.rounds) } }
              : {}),
          });
          const elapsedMs = knobs.validateOnly ? undefined : performance.now() - began;
          const checked = verify(cpu, definition, result, time, prepared);
          const ownersAfter = cpu.inspectTranslations().map((unit) => unit.owner);
          const physicalHash = definition.extended
            ? await fingerprint([
                Array.from(cpu.readPhysicalMemory(0n, RAM_BYTES)),
                Array.from(cpu.readPhysicalMemory(TABLES, 0x8000)),
              ])
            : undefined;
          const completedWorkHash = await fingerprint({
            state: result.state,
            paging: checked.paging,
            retired: result.retired,
            repeatIterations: result.repeatIterations,
            physicalHash,
          });
          paired.push(completedWorkHash);
          samples.push({
            side: label,
            name: definition.name,
            sample,
            ...(elapsedMs === undefined ? {} : { elapsedMs }),
            retired: result.retired,
            payloadAccesses: definition.payloadAccesses,
            descriptorReads: definition.descriptorReads,
            ...(definition.declaredWork ? { declaredWork: definition.declaredWork } : {}),
            invalidations: definition.invalidations,
            invalidationRequests: definition.invalidations,
            controlWrites: definition.controlWrites ?? 0n,
            noFlushWrites: definition.noFlushWrites ?? 0n,
            ...(result.deliveryAttempts === undefined
              ? {}
              : {
                  hostPendingDelivery: {
                    attempts: result.deliveryAttempts,
                    remainingBudget: result.remainingDeliveryBudget,
                  },
                }),
            primingRetired,
            beforeFirstLeaf,
            completedWorkHash,
            ...(physicalHash ? { physicalHash } : {}),
            publishedOwners: {
              before: ownersBefore,
              after: ownersAfter,
              observedRemovals: ownersBefore.filter((id) => !ownersAfter.includes(id)).length,
            },
            resources: result.resources,
            ...(knobs.validateOnly ? {} : { timings: result.timings }),
            ...checked,
          });
        } finally {
          cpu.dispose();
        }
      }
      if (baselineSide)
        equal(paired[0], paired[1], definition.name + ' paired completed-work hash');
    }
  }
  return {
    kind: baselineSide
      ? 'paired-common-ISA-retained-translation-characterization'
      : 'candidate-only-retained-translation-characterization',
    claimCeiling:
      'Paired results use only common-ISA guest work with identical required invalidations; new controls are candidate-only, never compared with Unsupported. No public TLB counters: authored payload/control counts are completed guest operations, while actual cache/walk work is proved separately by a private native trace. Owner removals are only before/after published-ID observations, not a count of transient owners inside one run. Resource compilations/native entries/generated bytes are actual public gauges.',
    schedulePolicy:
      'Seeded permutation per sample; each cell uses a fresh CPU, code priming and optional data priming outside elapsed run. No concurrent cells.',
    definitions,
    schedule,
    pairedSchedule,
    samples,
  };
}
