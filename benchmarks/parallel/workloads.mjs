const ENTRY = 0x1000n;
const RAM = 0x4000n;
const STEP = 0x1000n;
const limits = {
  maxInstructions: 128,
  maxSourceBytes: 2048,
  maxArtifactBytes: 262144,
  maxCachedArtifacts: 32,
  maxCachedBytes: 4194304,
  maxTranslationsPerRun: 128,
};
const definitions = {
  // Add RSI to RAX, decrement ECX, and follow the actual conditional backedge.
  alu: { code: '4801f0ffc975f9', retired: 3n },
  // Load and add adjacent qwords, write the sum back to the first lane.
  mixed: { code: '488b0348034308488903ffc975f2', retired: 5n },
  // Two disjoint qword stores per iteration, followed by a private register update.
  writes: { code: '488903488943084801f0ffc975f2', retired: 5n },
  // Atomic increments of the same shared qword by every worker.
  contended: { code: 'f048830301ffc975f7', retired: 3n },
};

function same(actual, expected, label) {
  if (actual !== expected) throw Error(`${label}: expected ${expected}, received ${actual}`);
}
function state(i, count, address) {
  const registers = Array(16).fill(0n);
  registers[0] = BigInt(i);
  registers[1] = BigInt(count);
  registers[3] = address;
  registers[6] = BigInt(i + 3);
  return { rip: ENTRY, flags: 2n, registers };
}
function data(i) {
  const bytes = new Uint8Array(16);
  const view = new DataView(bytes.buffer);
  view.setBigUint64(0, BigInt(i), true);
  view.setBigUint64(8, BigInt(i + 3), true);
  return bytes;
}
function readValue(bytes, offset) {
  return new DataView(bytes.buffer, bytes.byteOffset).getBigUint64(offset, true);
}
function median(values) {
  const sorted = values.slice().sort((a, b) => a - b);
  return (sorted[(sorted.length - 1) >> 1] + sorted[sorted.length >> 1]) / 2;
}

export async function measureParallel({
  createCpu,
  createMachine,
  parallelCore,
  soloCore,
  count,
  iterations,
  samples,
  workloads,
  reverse,
}) {
  if (![2, 4].includes(count)) throw Error('expected two or four workers');
  const cases = {};
  for (const name of workloads) {
    const { code: raw, retired } = definitions[name];
    const code = Uint8Array.from(raw.match(/../g), (byte) => Number.parseInt(byte, 16));
    const stop = ENTRY + BigInt(code.length);
    const addresses = Array.from({ length: count }, (_, i) => RAM + BigInt(i) * STEP);
    const end = Number(STEP) * count;
    const cpus = [];
    let machine;
    try {
      const began = performance.now();
      machine = await createMachine({
        compiler: parallelCore,
        memoryModel: 'linear',
        memory: {
          backings: [
            { id: 'code', bytes: code },
            { id: 'ram', bytes: new Uint8Array(end) },
          ],
          mappings: [
            {
              base: ENTRY,
              backing: 'code',
              offset: 0,
              length: code.length,
              permissions: { read: true, write: false, execute: true },
            },
            {
              base: RAM,
              backing: 'ram',
              offset: 0,
              length: end,
              permissions: { read: true, write: true, execute: false },
            },
          ],
        },
        memoryLimits: { maxBackings: 2, maxMappings: 2, maxBackingBytes: end + code.length },
        sharedRamCapacity: end + Number(STEP),
        limits,
        cpus: addresses.map(() => ({ entry: ENTRY })),
      });
      const sharedCreationMs = performance.now() - began;
      const constructing = performance.now();
      for (const address of addresses) {
        cpus.push(
          await createCpu({
            compiler: soloCore,
            code: { base: ENTRY, bytes: code },
            ram: { base: address, bytes: new Uint8Array(64) },
            limits,
          }),
        );
      }
      const privateCreationMs = performance.now() - constructing;
      async function run(mode, work) {
        const isPrivate = mode === 'privateSerial';
        const active = isPrivate ? cpus : machine.cpus;
        const want = BigInt(work) * retired;
        const actualAddresses =
          !isPrivate && name === 'contended' ? addresses.map(() => RAM) : addresses;
        if (!isPrivate && name === 'contended') await machine.writeMemory(RAM, new Uint8Array(16));
        for (const [i, cpu] of active.entries()) {
          const at = actualAddresses[i];
          await cpu.setState(state(i, work, at));
          if (isPrivate) cpu.writeMemory(at, name === 'contended' ? new Uint8Array(16) : data(i));
          else if (name !== 'contended') await machine.writeMemory(at, data(i));
        }
        const start = performance.now();
        const results = [];
        const options = { stop, budget: want + 1n };
        if (mode === 'sharedParallel')
          results.push(...(await Promise.all(active.map((cpu) => cpu.run(options)))));
        else for (const cpu of active) results.push(await cpu.run(options));
        const elapsedMs = performance.now() - start;
        const resources = [];
        for (const [i, result] of results.entries()) {
          same(result.reason, 'stop', name + ' completes');
          same(result.retired, want, name + ' matched work');
          same(result.state.rip, stop, name + ' endpoint');
          same(result.state.registers[1], 0n, name + ' loop counter');
          const value = BigInt(i) + BigInt(work) * BigInt(i + 3);
          if (name !== 'contended') same(result.state.registers[0], value, name + ' final rax');
          const at = actualAddresses[i];
          const ram = isPrivate ? active[i].readMemory(at, 16) : await machine.readMemory(at, 16);
          const lo = readValue(ram, 0),
            hi = readValue(ram, 8);
          if (name === 'alu' || name === 'mixed') {
            same(lo, name === 'alu' ? BigInt(i) : value, name + ' low lane');
            same(hi, BigInt(i + 3), name + ' preserved input lane');
          } else if (name === 'writes') {
            const last = value - BigInt(i + 3);
            same(lo, last, name + ' first output lane');
            same(hi, last, name + ' second output lane');
          } else {
            // Per-CPU private contended samples are independent controls, never a scaling denominator.
            same(lo, BigInt(work) * BigInt(isPrivate ? 1 : count), name + ' exact locked updates');
            same(hi, 0n, name + ' adjacent guard');
          }
          resources.push(result.resources);
        }
        return { mode, elapsedMs, work, totalRetired: Number(want) * count, resources };
      }
      const coldOrder = reverse
        ? ['privateSerial', 'sharedParallel']
        : ['sharedParallel', 'privateSerial'];
      const cold = [];
      for (const mode of coldOrder) cold.push(await run(mode, 1));
      const warm = [];
      const modes = ['sharedParallel', 'privateSerial', 'sharedSerial'];
      for (let i = 0; i < samples; i++) {
        const phase = (i + Number(reverse)) % modes.length;
        const order = [...modes.slice(phase), ...modes.slice(0, phase)];
        if (reverse) order.reverse();
        const rows = [];
        for (const mode of order) {
          const row = await run(mode, iterations);
          if (row.resources.some((resource) => resource.compilations !== 0)) {
            throw Error(name + ' supposedly warm run compiled');
          }
          rows.push(row);
        }
        warm.push(rows);
      }
      const summaries = Object.fromEntries(
        modes.map((mode) => [
          mode,
          median(warm.map((rows) => rows.find((row) => row.mode === mode).elapsedMs)),
        ]),
      );
      const parallel = summaries.sharedParallel;
      cases[name] = {
        source: raw,
        cold,
        warm,
        sharedCreationMs,
        privateCreationMs,
        summaries,
        sharedSpeedup: summaries.sharedSerial / parallel,
        privateBreakEven: name === 'contended' ? null : summaries.privateSerial / parallel,
      };
    } finally {
      for (const cpu of cpus) cpu.dispose();
      await machine?.dispose();
    }
  }
  return cases;
}

export const parallelWorkloads = Object.keys(definitions);
