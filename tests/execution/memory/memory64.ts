import { createCpu } from '../../../sdk/src/index.js';
import { equal, hex, initial, LIMITS, memory, throws } from '../../support/fixture.js';
import { BANK, little } from './paging/fixture.js';
import { mapping, RW, RX } from './mappings/fixture.js';

export function supportsMemory64(): boolean {
  // A module containing one i64-indexed memory with zero initial pages.
  return WebAssembly.validate(Uint8Array.of(0, 97, 115, 109, 1, 0, 0, 0, 5, 3, 1, 4, 0));
}

export async function checkMemory64(compiler: WebAssembly.Module) {
  const boundary = 2 ** 32;
  const entry = 0x1000n;
  const source = 0x8000n;
  const dest = 0x100008000n;
  const alias = 0x9000n;
  const patch = 0xa000n;
  const low = 0xb000n;
  const size = boundary + 128;
  // Copy dwords from RSI to RDI until ECX reaches zero.
  const code = hex('8b0689074883c6044883c704ffc975f0');
  const maps = [
    mapping(entry, 'code', code.length, RX),
    mapping(source, 'ram', 16, RW, boundary - 2),
    mapping(dest, 'ram', 16, RW, boundary + 32),
    mapping(alias, 'ram', 16, RX, boundary + 32),
    mapping(patch, 'ram', 5, RX, boundary + 64),
    mapping(patch + 16n, 'ram', 5, RW, boundary + 64),
    mapping(low, 'ram', 80, RW),
  ];
  const cpu = await createCpu({
    compiler,
    entry,
    limits: LIMITS,
    memoryLimits: { maxBackingBytes: size + code.length + 32, maxBackings: 3, maxMappings: 10 },
    memory: {
      backings: [
        { id: 'ram', bytes: hex('abcd'), byteLength: size },
        { id: 'code', bytes: code },
      ],
      mappings: maps,
    },
  });
  try {
    memory(cpu, low, 'abcd00');
    memory(cpu, dest, '00000000');
    const bytes = '0102030405060708090a0b0c0d0e0f10';
    cpu.writeMemory(source, hex(bytes));
    for (let i = 0; i < 2; i++) {
      cpu.setState(initial(entry, { 1: 4n, 6: source, 7: dest }));
      equal(
        (await cpu.run({ budget: 50n, stop: entry + BigInt(code.length) })).reason,
        'stop',
        'high x86 copy',
      );
      memory(cpu, alias, bytes);
      memory(cpu, low + 32n, '00000000');
    }
    // This extent begins above 4 GiB in Wasm and has no executable aliases.
    // The compiled loop may cache its writable span.
    cpu.addBacking({ id: 'sink', bytes: new Uint8Array(16) });
    cpu.setMappings(maps.map((m) => (m.base === dest ? mapping(dest, 'sink', 16, RW) : m)));
    cpu.writeMemory(source, hex('1102030405060708090a0b0c0d0e0f1f'));
    cpu.setState(initial(entry, { 1: 4n, 6: source, 7: dest }));
    equal(
      (await cpu.run({ budget: 50n, stop: entry + BigInt(code.length) })).reason,
      'stop',
      'direct high write',
    );
    memory(cpu, dest, '1102030405060708090a0b0c0d0e0f1f');
    memory(cpu, alias, bytes);
    cpu.setState(initial(entry, { 1: 1n, 6: source, 7: dest + 14n }));
    equal((await cpu.run({ budget: 10n })).reason, 'data-fault', 'cross-mapping x86 store');
    memory(cpu, dest + 14n, '0f1f');
    cpu.setMappings(maps);
    cpu.removeBacking('sink');
    throws(() => cpu.writeMemory(dest + 15n, hex('bbcc')), 'split unmapped store');
    memory(cpu, dest + 15n, '10');
    throws(() => cpu.writeMemory(alias, hex('cc')), 'write to RX alias');
    // A write through the RW alias must retire the old translation at the RX alias.
    for (const value of [1, 2]) {
      cpu.writeMemory(patch + 16n, Uint8Array.of(0xb8, value, 0, 0, 0));
      cpu.setState(initial(patch));
      const step = await cpu.run({ budget: 1n });
      equal(step.state.registers[0], BigInt(value), 'high executable alias invalidation');
    }
    cpu.setMappings([...maps.slice(0, 3), mapping(alias, 'ram', 2, RX), ...maps.slice(4)]);
    memory(cpu, alias, 'abcd');
    // Released extents start out zero even when the arena reuses the same bytes.
    for (const value of [0xaa, 0xbb]) {
      cpu.addBacking({ id: 'temp', bytes: Uint8Array.of(value), byteLength: 32 });
      cpu.setMappings([...maps, mapping(0xc000n, 'temp', 32, RW)]);
      memory(cpu, 0xc000n, value.toString(16) + '00');
      cpu.writeMemory(0xc001n, hex('ff'));
      cpu.setMappings(maps);
      cpu.removeBacking('temp');
    }
  } finally {
    cpu.dispose();
  }
  await checkPagedMemory64(compiler);
}

async function checkPagedMemory64(compiler: WebAssembly.Module) {
  const boundary = 2 ** 32;
  const physical = 0x100000000n;
  const entry = 0x400000n;
  const tables = new Uint8Array(0x4000);
  for (const [offset, pte] of [
    [0, 0x2007n],
    [0x1000, 0x3007n],
    [0x2010, 0x4007n],
    [0x3000, 0x9007n],
    [0x3008, physical | 7n],
  ] as const)
    tables.set(little([pte]), offset);
  const size = boundary + 4096;
  const cpu = await createCpu({
    compiler,
    entry,
    limits: LIMITS,
    memoryModel: 'ia32e-paged',
    paging: BANK,
    memoryLimits: { maxBackingBytes: size + tables.length + 4, maxBackings: 3, maxMappings: 3 },
    memory: {
      backings: [
        { id: 'tables', bytes: tables },
        { id: 'data', bytes: new Uint8Array(0), byteLength: size },
        { id: 'code', bytes: hex('89068b06') },
      ],
      mappings: [
        mapping(0x1000n, 'tables', tables.length, RW),
        mapping(physical, 'data', 4096, RW, boundary),
        mapping(0x9000n, 'code', 4, RX),
      ],
    },
  });
  try {
    cpu.setState(initial(entry, { 0: 0x12345678n, 6: entry + 4096n }));
    const result = await cpu.run({ budget: 3n, stop: entry + 4n });
    equal(result.reason, 'stop', 'paged memory64 access');
    equal(result.state.registers[0], 0x12345678n, 'paged high dword');
    equal(cpu.readPhysicalMemory(physical, 1)[0], 0x78, 'high physical backing');
    equal(cpu.readPhysicalMemory(0x4008n, 1)[0]! & 0x60, 0x60, 'accessed and dirty leaf');
  } finally {
    cpu.dispose();
  }
}
