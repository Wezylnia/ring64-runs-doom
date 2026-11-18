import { createCpu, type RunResult } from '../../../host/index.js';
import { LIMITS } from '../../support/fixture.js';
import {
  bytes,
  equal,
  fault,
  hex,
  input,
  memory,
  pop,
  stack,
  x87,
  type Scenario,
} from './fixture.js';

// Relocation-free complete SinCos symbols from Debian fp-units-rtl-3.2.2_3.2.2+dfsg-20_amd64's math.o.
// Source: https://gitlab.com/freepascal.org/fpc/source/-/blob/release_3_2_2/rtl/x86_64/mathu.inc
// Copyright (c) 1999–2004 Florian Klaempfl. Free Pascal RTL, see COPYING.FPC in that release.
const bodies = {
  extended: { code: '554889e5db6d10d9fbdb3edb3f9b4889ec5dc3', before: 3n, op: 7n, width: 10 },
  double: {
    code: '554889e5488d6424f0f20f1145f8dd45f8d9fbdd1edd1f9b4889ec5dc3',
    before: 5n,
    op: 17n,
    width: 8,
  },
  single: {
    code: '554889e5488d6424f0f30f1145fcd945fcd9fbd91ed91f9b4889ec5dc3',
    before: 5n,
    op: 17n,
    width: 4,
  },
} as const;

const pc = 0x1000n,
  stop = 0x2000n,
  stackBase = 0x4000n,
  rsp = stackBase + 72n,
  sine = 0x5ffen,
  cosine = 0x7ffen;
const outputs = { sine, cosine } as const;
type Output = keyof typeof outputs;

// MPFR nearest from the exact dyadic 1/2, at each destination precision.
const anchors = {
  extended: [0x3ffe8000000000000000n, 0x3ffdf57743a2582f7f44n, 0x3ffee0a94032dbea7ceen],
  double: [0x3fe0000000000000n, 0x3fdeaee8744b05f0n, 0x3fec1528065b7d50n],
  single: [0x3f000000n, 0x3ef57744n, 0x3f60a940n],
} as const;

export type FreePascalSinCosOverload = keyof typeof bodies;

export async function createFreePascalSinCos(
  compiler: WebAssembly.Module,
  overload: FreePascalSinCosOverload,
  fail?: Output,
) {
  const { code, before, op, width } = bodies[overload];
  const codeBytes = hex(code);
  const backings = [
    { id: 'code', bytes: codeBytes },
    { id: 'stack', bytes: new Uint8Array(128) },
    ...Object.keys(outputs).map((id) => ({ id, bytes: new Uint8Array(width + 8).fill(0xa5) })),
  ];
  const mappings = (guard?: Output) => [
    {
      base: pc,
      backing: 'code',
      offset: 0,
      length: codeBytes.length,
      permissions: { read: true, write: false, execute: true },
    },
    {
      base: stackBase,
      backing: 'stack',
      offset: 0,
      length: 128,
      permissions: { read: true, write: true, execute: false },
    },
    ...Object.entries(outputs).flatMap(([name, address]) => [
      {
        base: address - 4n,
        backing: name,
        offset: 0,
        length: 6,
        permissions: { read: true, write: true, execute: false },
      },
      {
        base: address + 2n,
        backing: name,
        offset: 6,
        length: width + 2,
        permissions: { read: true, write: name !== guard, execute: false },
      },
    ]),
  ];
  const cpu = await createCpu({
    compiler,
    entry: pc,
    memory: { backings, mappings: mappings(fail) },
    memoryLimits: { maxBackings: 4, maxMappings: 6, maxBackingBytes: 256 },
    limits: LIMITS,
  });
  const caller = input(pc, { 4: rsp, 5: 0x123456789abcdefn, 6: cosine, 7: sine }, 0x8d7n);
  const xmm = Array.from({ length: 16 }, (_, i) => (1n << 127n) + BigInt(i));
  const frame = new Uint8Array(128);
  const untouched = new Uint8Array(width).fill(0xa5);
  let argument: bigint;

  function prepare(value: bigint = anchors[overload][0], cw = 0x37f) {
    argument = value;
    if (fail === undefined) {
      cpu.writeMemory(sine, untouched);
      cpu.writeMemory(cosine, untouched);
    }
    frame.fill(0xa5);
    const view = new DataView(frame.buffer);
    view.setBigUint64(72, stop, true);
    if (width === 10) frame.set(hex(bytes(argument, width)), 80);
    cpu.writeMemory(stackBase, frame);
    view.setBigUint64(64, caller.registers[5]!, true);
    if (width !== 10) frame.set(hex(bytes(argument, width)), 64 - width);
    cpu.setState(caller);
    cpu.setX87State(stack({}, 0, cw));
    cpu.setXmmState(width === 10 ? xmm : [xmm[0]! | argument, ...xmm.slice(1)]);
  }

  function checkOutput(name: Output, value?: bigint) {
    memory(
      cpu,
      outputs[name] - 4n,
      'a5'.repeat(4) +
        (value === undefined ? 'a5'.repeat(width) : bytes(value, width)) +
        'a5'.repeat(4),
    );
  }

  function checkComplete(result: RunResult, retired = before + 7n) {
    equal(result.reason, 'stop', overload + ' original SinCos returns');
    equal(result.retired, retired, 'retired exactly through RET');
    equal(result.state.rip, stop, 'RET used the caller slot');
    equal(result.state.flags, caller.flags, 'RFLAGS');
    for (const [i, value] of caller.registers.entries())
      equal(result.state.registers[i], i === 4 ? rsp + 8n : value, 'caller register ' + i);
    const numeric = cpu.readX87State();
    equal(numeric.tagWord, 0xffff, 'both FSTP instructions popped');
    equal((numeric.statusWord >> 11) & 7, 0, 'restored x87 TOP');
    for (const [i, value] of cpu.readXmmState().entries())
      equal(value, i === 0 && width !== 10 ? xmm[0]! | argument : xmm[i], 'XMM' + i);
    for (const [i, byte] of cpu.readMemory(stackBase, frame.length).entries())
      equal(byte, frame[i], 'stack frame byte ' + i);
  }

  return {
    cpu,
    prepare,
    checkOutput,
    checkComplete,
    stop,
    before,
    producer: pc + op,
    width,
    repair: () => cpu.setMappings(mappings()),
  };
}

// Independent bit rounding for the normal binary80 values in the narrowing witness.
function narrow(value: bigint, width: number, rc: number): bigint {
  const precision = width === 4 ? 24 : 53;
  const bias = width === 4 ? 127 : 1023;
  const bits = BigInt(precision - 1);
  const drop = BigInt(64 - precision);
  const exponent = Number((value >> 64n) & 0x7fffn) - 16383 + bias;
  const negative = value >> 79n === 1n;
  const significand = BigInt.asUintN(64, value);
  const low = significand & ((1n << drop) - 1n);
  const high = significand >> drop;
  const half = 1n << (drop - 1n);
  const increment =
    rc === 0
      ? low > half || (low === half && (high & 1n) === 1n)
      : low !== 0n && (rc === 1 ? negative : rc === 2 && !negative);
  equal(exponent > 0 && exponent < 2 * bias + 1, true, 'normal narrowing witness');
  return (
    (negative ? 1n << BigInt(width * 8 - 1) : 0n) |
    ((BigInt(exponent - 1) << bits) + high + BigInt(increment))
  );
}

export function freePascalSinCosScenarios(compiler: WebAssembly.Module): Scenario[] {
  const overloads = ['extended', 'double', 'single'] as const;
  return [
    {
      name: 'original Free Pascal SinCos overloads return both results in their actual ABI formats',
      async run() {
        for (const overload of overloads) {
          const f = await createFreePascalSinCos(compiler, overload);
          try {
            const [half, sinHalf, cosHalf] = anchors[overload];
            const sign = 1n << BigInt(f.width * 8 - 1);
            const one =
              overload === 'extended'
                ? 0x3fff8000000000000000n
                : overload === 'double'
                  ? 0x3ff0000000000000n
                  : 0x3f800000n;
            for (const [value, sin, cos] of [
              [half, sinHalf, cosHalf],
              [half | sign, sinHalf | sign, cosHalf],
              [sign, sign, one],
            ]) {
              f.prepare(value);
              f.checkComplete(await f.cpu.run({ budget: 32n, stop: f.stop }));
              f.checkOutput('sine', sin);
              f.checkOutput('cosine', cos);
            }
          } finally {
            f.cpu.dispose();
          }
        }
      },
    },
    {
      name: 'original Free Pascal FSTP narrows the produced binary80 pair under live directed rounding',
      async run() {
        for (const overload of ['double', 'single'] as const) {
          const f = await createFreePascalSinCos(compiler, overload);
          try {
            const sign = 1n << BigInt(f.width * 8 - 1);
            for (const rc of [1, 2]) {
              f.prepare(sign | anchors[overload][0], 0x37f | (rc << 10));
              const reached = await f.cpu.run({ budget: 32n, stop: f.producer + 2n });
              equal(reached.reason, 'stop', 'stop before the first FSTP');
              equal(reached.retired, f.before + 1n, 'one producer retirement');
              const pair = f.cpu.readX87State();
              equal((pair.statusWord >> 11) & 7, 6, 'two x87 results');
              f.checkOutput('sine');
              f.checkOutput('cosine');
              f.checkComplete(await f.cpu.run({ budget: 32n, stop: f.stop }), 6n);
              f.checkOutput('cosine', narrow(pair.registers[6]!, f.width, rc));
              f.checkOutput('sine', narrow(pair.registers[7]!, f.width, rc));
            }
          } finally {
            f.cpu.dispose();
          }
        }
      },
    },
    {
      name: 'each original Free Pascal FSTP can fault mid-store and retry without replaying the pair',
      async run() {
        for (const overload of overloads)
          for (const failed of ['cosine', 'sine'] as const) {
            const f = await createFreePascalSinCos(compiler, overload, failed);
            try {
              f.prepare();
              const stopped = await f.cpu.run({ budget: 32n, stop: f.producer + 2n });
              equal(stopped.reason, 'stop', 'stop after the producer');
              equal(stopped.retired, f.before + 1n, 'prologue and producer retired');
              const pair = f.cpu.readX87State();
              const failedAt = f.producer + (failed === 'cosine' ? 2n : 4n);
              const r = await f.cpu.run({ budget: 32n, stop: f.stop });
              equal(r.reason, 'data-fault', 'selected output fault');
              equal(r.retired, failed === 'cosine' ? 0n : 1n, 'only earlier FSTP can retire');
              equal(r.state.rip, failedAt, 'fault RIP identifies current FSTP');
              fault(r, {
                instructionPc: failedAt,
                address: outputs[failed],
                width: f.width,
                access: 'write',
                cause: 'permission',
                failingAddress: outputs[failed] + 2n,
                segment: 'ds',
              });
              const [, sinHalf, cosHalf] = anchors[overload];
              f.checkOutput('sine');
              if (failed === 'sine') {
                f.checkOutput('cosine', cosHalf);
                pop(pair);
                pair.instructionPointer = f.producer + 2n;
                pair.dataPointer = cosine;
                // Narrow FSTP contributes its own C1, independent of FSINCOS.
                pair.statusWord =
                  (pair.statusWord & ~0x0200) |
                  (f.width !== 10 && narrowMagnitudeIncrement(pair.registers[6]!, f.width, cosHalf)
                    ? 0x0200
                    : 0);
              } else {
                f.checkOutput('cosine');
              }
              x87(f.cpu, pair);
              f.repair();
              f.checkComplete(
                await f.cpu.run({ budget: 32n, stop: f.stop }),
                failed === 'cosine' ? 6n : 5n,
              );
              f.checkOutput('sine', sinHalf);
              f.checkOutput('cosine', cosHalf);
            } finally {
              f.cpu.dispose();
            }
          }
      },
    },
  ];
}

function narrowMagnitudeIncrement(raw80: bigint, width: number, narrowed: bigint) {
  const bits = width === 4 ? 24 : 53;
  const mask = (1n << BigInt(bits - 1)) - 1n;
  return (narrowed & mask) > ((raw80 >> BigInt(64 - bits)) & mask);
}
