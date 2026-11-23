import { createCpu } from '../../../sdk/src/index.js';
import { observeArtifactImports } from '../../support/artifact-imports.js';
import { equal, hex, initial, LIMITS, memory, same, throws } from '../../support/fixture.js';
import { BANK, little, word } from './paging/fixture.js';
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
  const data = entry + 4096n;
  const readonlyAlias = data + 4096n;
  const tables = new Uint8Array(0x4000);
  for (const [offset, pte] of [
    [0, 0x2007n],
    [0x1000, 0x3007n],
    [0x2010, 0x4007n],
    [0x3000, 0x9007n],
    [0x3008, physical | 7n],
    [0x3010, physical | 5n],
  ] as const)
    tables.set(little([pte]), offset);
  // MOV EDX,[RDI]; INC ECX; MOV [RSI],EAX; XOR EAX,5; MOV EBX,[RDI]; INC EDX; HLT.
  const code = hex('8b17ffc1890683f0058b1fffc2f4');
  const stop = entry + BigInt(code.length - 1);
  const size = boundary + 4096;
  const mappings = (offset: number) => [
    mapping(0x1000n, 'tables', tables.length, RW),
    mapping(physical, 'ram', 4096, RW, offset),
    mapping(physical + 4096n, 'ram', 4096, RW, boundary - offset),
    mapping(0x9000n, 'code', code.length, RX),
  ];
  const cpu = await createCpu({
    compiler,
    entry,
    limits: LIMITS,
    memoryModel: 'ia32e-paged',
    paging: { ...BANK, cr0: BANK.cr0 | 0x10000n },
    memoryLimits: {
      maxBackingBytes: size + tables.length + code.length,
      maxBackings: 3,
      maxMappings: 4,
    },
    memory: {
      backings: [
        { id: 'tables', bytes: tables },
        { id: 'ram', bytes: new Uint8Array(0), byteLength: size },
        { id: 'code', bytes: code },
      ],
      mappings: mappings(0),
    },
  });
  let checked = 0;
  const restore = observeArtifactImports((env) => {
    for (const key of ['load', 'store']) {
      const operand = env[key];
      if (typeof operand === 'function')
        env[key] = (...args: (number | bigint)[]) => {
          checked++;
          return operand(...args);
        };
    }
  });
  const run = (value: bigint, destination = data + 4n) => {
    cpu.setState(initial(entry, { 0: value, 1: 7n, 6: destination, 7: data }));
    return cpu.run({ budget: 8n, stop });
  };
  try {
    cpu.writePhysicalMemory(physical, little([19n, 0xaabbccddn], 4));
    cpu.writePhysicalMemory(physical + 4096n, little([37n, 0x11223344n], 4));
    const warmCalls: number[] = [];
    // The same virtual page resolves to two backing offsets with identical low 32 bits.
    for (const [offset, source, marker, other] of [
      [0, 19n, 0xfedcba98n, 0x11223344n],
      [boundary, 37n, 0x12345678n, 0xfedcba98n],
    ] as const) {
      if (offset !== 0) cpu.setMappings(mappings(offset));
      equal((await run(1n)).reason, 'stop', 'cold paged access');
      checked = 0;
      const warm = await run(marker);
      warmCalls.push(checked);
      equal(warm.reason, 'stop', 'warm paged access');
      equal(warm.retired, 6n, 'all instructions retire');
      equal(warm.state.registers[2], source + 1n, 'read from the current backing offset');
      equal(warm.state.registers[3], source, 'second read agrees');
      equal(word(cpu, physical), (marker << 32n) | source, 'full dword write to current offset');
      equal(word(cpu, physical + 4096n) >> 32n, other, 'other offset preserves its bytes');
    }
    equal(cpu.readPhysicalMemory(0x4008n, 1)[0]! & 0x60, 0x60, 'accessed and dirty writable leaf');
    const fault = await run(0xffn, readonlyAlias + 4n);
    equal(fault.reason, 'page-fault', 'read-only alias refuses a warmed write');
    equal(fault.pageFault?.address, readonlyAlias + 4n, 'faulting linear address');
    equal(fault.pageFault?.errorCode, 3, 'supervisor write protection');
    equal(fault.state.rip, entry + 4n, 'fault attributed to the store');
    equal(fault.retired, 2n, 'successful load and increment precede the fault');
    equal(word(cpu, physical) >> 32n, 0x12345678n, 'denied store leaves bytes intact');
    equal(cpu.readPhysicalMemory(0x4010n, 1)[0]! & 0x40, 0, 'denied alias stays clean');
    same(
      warmCalls,
      [0, 0],
      'warm paged reads and writes avoid per-operand fallback below and above 4 GiB',
    );
  } finally {
    restore();
    cpu.dispose();
  }
}
