const PC = 0x1000n,
  DATA = 0x6000n,
  STACK = 0x8000n;
const hex = (value) => Uint8Array.from(value.match(/../g), (byte) => parseInt(byte, 16));
const encode = (value) =>
  JSON.stringify(value, (_, item) =>
    typeof item === 'bigint'
      ? item.toString()
      : item && typeof item === 'object' && !Array.isArray(item)
        ? Object.fromEntries(Object.entries(item).sort(([a], [b]) => a.localeCompare(b)))
        : item,
  );
const equal = (actual, expected, label) => {
  if (encode(actual) !== encode(expected))
    throw Error(`${label}: ${encode(actual)} != ${encode(expected)}`);
};
const put = (bytes, offset, value, width) => {
  for (let i = 0; i < width; i++) bytes[offset + i] = Number((value >> BigInt(i * 8)) & 255n);
};
const rows = [
  { name: 'load', body: '8b03', instructions: 3 },
  { name: 'store', body: '8903', instructions: 3 },
  { name: 'rmw', body: '830301', instructions: 3 },
  { name: 'stack', body: '5058', instructions: 4 },
  { name: 'rep', body: 'f3a5', instructions: 1, repeat: true },
  { name: 'environment', body: 'd933', instructions: 3 },
  { name: 'fxsave', body: '0fae03', instructions: 3 },
];
export const alignmentPerformanceNames = rows.map((row) => row.name);
const controls = [
  { name: 'active-aligned', flags: 0x40002n, cpl: 3 },
  { name: 'ac-disabled', flags: 2n, cpl: 3 },
  { name: 'supervisor', flags: 0x40002n, cpl: 0 },
];
export function alignmentPerformancePlan({
  iterations = 100001,
  samples = 11,
  workloads = alignmentPerformanceNames,
  seed = 1701,
} = {}) {
  for (const [name, value, max] of [
    ['iterations', iterations, 100001],
    ['samples', samples, 101],
    ['seed', seed, 0xffffffff],
  ])
    if (!Number.isSafeInteger(value) || value < 1 || value > max)
      throw Error(`${name} outside bounded range`);
  if (
    !workloads.length ||
    new Set(workloads).size !== workloads.length ||
    workloads.some((name) => !alignmentPerformanceNames.includes(name))
  )
    throw Error('unknown/duplicate workload');
  const definitions = [];
  for (const model of ['physical-pg0', 'fixed-ia32e'])
    for (const row of rows.filter((row) => workloads.includes(row.name)))
      for (const control of controls) {
        const long = model === 'fixed-ia32e';
        const loop = long ? 'ffc9' : '49';
        const length = row.body.length / 2 + loop.length / 2 + 2;
        const bytes = hex(
          row.repeat
            ? row.body
            : row.body + loop + '75' + (256 - length).toString(16).padStart(2, '0'),
        );
        definitions.push({
          ...control,
          name: `${model}/${row.name}/${control.name}`,
          control: control.name,
          model,
          kind: row.name,
          iterations,
          samples,
          seed,
          code: Array.from(bytes),
          retired: row.repeat ? 1n : BigInt(row.instructions * iterations),
          elements: row.repeat ? BigInt(iterations) : 0n,
        });
      }
  return definitions;
}
function image(definition) {
  const bytes = new Uint8Array(Math.max(0xa000, 0x6020 + definition.iterations * 8)).fill(0xa5);
  bytes.set(definition.code, Number(PC));
  if (definition.kind === 'rep') {
    for (let i = 0; i < definition.iterations * 4; i++)
      bytes[Number(DATA) + i] = (i * 17 + 3) & 255;
  } else put(bytes, Number(DATA), 7n, 4);
  return bytes;
}
function expectedBytes(before, definition) {
  const bytes = before.slice(),
    at = Number(DATA),
    long = definition.model === 'fixed-ia32e';
  if (definition.kind === 'rmw') put(bytes, at, BigInt(definition.iterations + 7), 4);
  if (definition.kind === 'stack') put(bytes, Number(STACK) - (long ? 8 : 4), 7n, long ? 8 : 4);
  if (definition.kind === 'rep')
    bytes.copyWithin(at + definition.iterations * 4 + 16, at, at + definition.iterations * 4);
  if (definition.kind === 'environment') {
    put(bytes, at, 0x37fn, 2);
    put(bytes, at + 4, 0n, 2);
    put(bytes, at + 8, 0xffffn, 2);
    bytes.fill(0, at + 12, at + 26);
  }
  if (definition.kind === 'fxsave') {
    bytes.fill(0, at, at + 5);
    put(bytes, at, 0x37fn, 2);
    bytes.fill(0, at + 6, at + 14);
    bytes.fill(0, at + 16, at + 22);
    put(bytes, at + 24, 0x1f80n, 4);
    put(bytes, at + 28, 0xffffn, 4);
    for (let slot = 0; slot < 8; slot++) bytes.fill(0, at + 32 + slot * 16, at + 42 + slot * 16);
    bytes.fill(0, at + 160, at + (long ? 416 : 288));
  }
  return bytes;
}
async function fixture(side, definition, am) {
  const ram = image(definition),
    long = definition.model === 'fixed-ia32e';
  const permissions = { read: true, write: true, execute: true };
  const backings = [{ id: 'ram', bytes: ram }];
  const mappings = [{ base: 0n, backing: 'ram', offset: 0, length: ram.length, permissions }];
  let tables;
  if (long) {
    tables = new Uint8Array(0x3000);
    put(tables, 0, 0x101027n, 8);
    put(tables, 0x1000, 0x102027n, 8);
    put(tables, 0x2000, 0xe7n, 8);
    backings.push({ id: 'tables', bytes: tables });
    mappings.push({
      base: 0x100000n,
      backing: 'tables',
      offset: 0,
      length: tables.length,
      permissions: { ...permissions, execute: false },
    });
  }
  const segment = (type) => ({ base: 0n, limit: 0xffffffff, default32: true, usable: true, type });
  const context = long
    ? { mode: 'long64', fsBase: 0n, gsBase: 0n }
    : {
        mode: 'protected',
        segments: {
          cs: segment('readable-code'),
          ss: segment('read-write'),
          ds: segment('read-write'),
          es: segment('read-write'),
          fs: segment('read-write'),
          gs: segment('read-write'),
        },
      };
  const paging = {
    cr0: (long ? 0x80000033n : 0x11n) | (am ? 0x40000n : 0n),
    cr2: 0xdeadn,
    cr3: long ? 0x100000n : 0n,
    cr4: long ? 0x628n : 0n,
    efer: long ? 0x500n : 0n,
  };
  const cpu = await side.createCpu({
    compiler: side.compiler,
    memoryModel: long ? 'ia32e-paged' : 'physical',
    paging,
    context,
    entry: PC,
    memory: { backings, mappings },
    memoryLimits: {
      maxBackings: 2,
      maxMappings: 2,
      maxBackingBytes: ram.length + (tables?.length ?? 0),
    },
    limits: {
      maxInstructions: 32,
      maxSourceBytes: 256,
      maxArtifactBytes: 65536,
      maxCachedArtifacts: 8,
      maxCachedBytes: 1048576,
      maxTranslationsPerRun: 8,
    },
  });
  try {
    const reached = cpu.readContext();
    reached.cpl = definition.cpl;
    for (const [name, segment] of Object.entries(reached.segments)) {
      segment.dpl = definition.cpl;
      segment.selector = (segment.selector & ~3) | definition.cpl;
      if (name === 'cs') segment.conforming = false;
    }
    cpu.setContext(reached);
    const expectedContext = cpu.readContext(),
      numeric = [cpu.readX87State(), cpu.readXmmState(), cpu.readMxcsr()],
      debug = cpu.readDebugState();
    const registers = Array.from({ length: 16 }, (_, index) => BigInt(index + 0x100));
    registers[0] = 7n;
    registers[1] = BigInt(definition.iterations);
    registers[3] = DATA;
    registers[4] = STACK;
    registers[6] = DATA;
    registers[7] = DATA + BigInt(definition.iterations * 4 + 16);
    const initial = {
      registers,
      rip: PC,
      flags: definition.flags,
      interruptibility: { sti: false, ss: false },
      event: { kind: 'ready' },
      interrupts: { irq: null, nmiPending: false, nmiBlocked: false },
    };
    const expected = {
      ...initial,
      registers: [...registers],
      rip: PC + BigInt(definition.code.length),
      flags: definition.kind === 'rep' ? definition.flags : (definition.flags & 0x40000n) | 0x46n,
    };
    if (definition.kind !== 'rep') expected.registers[1] = 0n;
    else {
      expected.registers[1] = 0n;
      expected.registers[6] += BigInt(definition.iterations * 4);
      expected.registers[7] += BigInt(definition.iterations * 4);
    }
    const expectedRam = expectedBytes(ram, definition);
    const rearm = () => {
      cpu.writePhysicalMemory(DATA, ram.slice(Number(DATA)));
      cpu.setState(initial);
    };
    const verify = (result, beforeTime, warm) => {
      equal(result.reason, 'budget', definition.name + ' reason');
      equal(result.retired, definition.retired, 'retired');
      equal(result.remaining, 0n, 'remaining');
      equal(result.repeatIterations, definition.elements, 'elements');
      equal(result.state, expected, definition.name + ' full scalar');
      equal(cpu.readContext(), expectedContext, 'complete context');
      equal(cpu.readPagingState(), paging, 'complete controls');
      equal([cpu.readX87State(), cpu.readXmmState(), cpu.readMxcsr()], numeric, 'numeric banks');
      equal(cpu.readDebugState(), debug, 'debug bank');
      equal(
        cpu.readTimeState(),
        { tsc: beforeTime.tsc + definition.retired, tscAux: beforeTime.tscAux },
        'retirement time',
      );
      const actualRam = cpu.readPhysicalMemory(0n, ram.length);
      for (let i = 0; i < ram.length; i++)
        if (actualRam[i] !== expectedRam[i])
          throw Error(`${definition.name} physical byte${i.toString(16)}`);
      if (tables)
        equal(
          Array.from(cpu.readPhysicalMemory(0x100000n, tables.length)),
          Array.from(tables),
          'warm page words',
        );
      if (warm) equal(result.resources.compilations, 0, 'warm source reuse');
      for (const name of [
        'alignmentCheck',
        'alignmentFault',
        'fault',
        'pageFault',
        'physicalMemoryFault',
        'protectionFault',
        'systemMemoryFault',
        'simdException',
        'x87Exception',
        'debugException',
      ])
        equal(result[name], undefined, 'no hidden ' + name);
    };
    return { cpu, rearm, verify };
  } catch (error) {
    cpu.dispose();
    throw error;
  }
}
export async function measureAlignmentPerformance(side, knobs = {}) {
  const definitions = alignmentPerformancePlan(knobs),
    samples = [],
    schedule = [];
  let random = knobs.seed ?? 1701;
  for (const definition of definitions) {
    const active = [];
    try {
      for (const am of [false, true])
        active.push({ ...(await fixture(side, definition, am)), label: am ? 'AM1' : 'AM0' });
      for (const item of active) {
        item.rearm();
        const beforeTime = item.cpu.readTimeState();
        const result = await item.cpu.run({
          budget: definition.retired,
          repeatBudget: definition.elements,
        });
        item.verify(result, beforeTime, false);
      }
      random ^= random << 13;
      random ^= random >>> 17;
      random ^= random << 5;
      for (let sample = 0; sample < definition.samples; sample++) {
        const order = ((random >>> 0) + sample) & 1 ? [...active].reverse() : active;
        for (const item of order) {
          item.rearm();
          const beforeTime = item.cpu.readTimeState();
          const began = knobs.validateOnly ? 0 : performance.now();
          const result = await item.cpu.run({
            budget: definition.retired,
            repeatBudget: definition.elements,
          });
          const elapsedMs = knobs.validateOnly ? undefined : performance.now() - began;
          item.verify(result, beforeTime, true);
          schedule.push({ workload: definition.name, sample, side: item.label });
          samples.push({
            workload: definition.name,
            sample,
            side: item.label,
            elapsedMs,
            retired: result.retired,
            elements: result.repeatIterations,
            resources: result.resources,
            timings: result.timings,
          });
        }
      }
    } finally {
      for (const item of active) item.cpu.dispose();
    }
  }
  return {
    definitions,
    samples,
    schedule,
    baseline: null,
    comparison: 'same candidate AM1/AM0 with identical completed work and separate AC/CPL controls',
    timing: 'awaited run only; warm-up, RAM/state rearm and full-state/RAM guards excluded',
    schedulePolicy:
      'seeded first side per cohort, then alternating AB/BA; each arm has its own CPU and source owners',
  };
}
