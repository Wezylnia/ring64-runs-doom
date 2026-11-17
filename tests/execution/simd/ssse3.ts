import { BANK, PC, usingPaged } from '../paging/fixture.js';
import {
  LONG,
  OTHER_MXCSR,
  banks,
  bytes,
  changed,
  clone,
  equal,
  input,
  noFault,
  outcome,
  pending,
  stack,
  using,
  x87,
  xmm,
  type Scenario,
} from '../x87/fixture.js';
import { packets } from './ssse3-packets.js';

const data = 0x6000n;
type Packet = {
  map: string;
  opcode: string;
  immediate: string;
  left: bigint;
  right: bigint;
  result: bigint;
  self: bigint;
};

async function checkPacket(compiler: WebAssembly.Module, wide: boolean, packet: Packet) {
  // REX.R/B selects XMM8/9, is ignored for MMX registers, and extends the SIB base to R11.
  const opcode = (wide ? '66' : '') + '450f' + packet.map + packet.opcode;
  const forms = ['c1', 'c0', '4453f0'].map((operand) => opcode + operand + packet.immediate);
  const width = wide ? 16 : 8;
  await using(
    compiler,
    forms.join(''),
    LONG,
    async (cpu) => {
      let pc = 0x1000n;
      for (const [i, code] of forms.entries()) {
        const before = input(pc, { 11: data, 2: 8n });
        const numeric = stack({}, 5);
        const vector = banks();
        if (wide) {
          vector[8] = packet.left;
          vector[9] = packet.right;
        } else {
          numeric.registers[0] = (0x1234n << 64n) | BigInt.asUintN(64, packet.left);
          numeric.registers[1] = (0x5678n << 64n) | BigInt.asUintN(64, packet.right);
        }
        cpu.setX87State(numeric);
        cpu.setXmmState(vector);
        cpu.setMxcsr(OTHER_MXCSR);
        cpu.setState(before);
        pc += BigInt(code.length / 2);
        const result = await cpu.run({ budget: 1n });
        outcome(result, 'budget', 1n, 1n, 1n, 0n, changed(before, pc));
        noFault(result);
        const expected = i === 1 ? packet.self : packet.result;
        if (wide) vector[8] = expected;
        else {
          numeric.registers[0] = (0xffffn << 64n) | expected;
          numeric.tagWord = 0;
          numeric.statusWord &= ~0x3800;
        }
        x87(cpu, numeric);
        xmm(cpu, vector);
        equal(cpu.readMxcsr(), OTHER_MXCSR, 'packed integer MXCSR');
      }
    },
    { data: [{ base: data, bytes: bytes(packet.right, width), write: false }] },
  );
}

export function ssse3Scenarios(compiler: WebAssembly.Module): Scenario[] {
  const scenarios: Scenario[] = packets.map(
    ([name, opcode, left, right, mmxResult, xmmResult, mmxSelf, xmmSelf]) => ({
      name: 'SSSE3 ' + name + ' matches x86 register, alias and memory packets in both banks',
      async run() {
        for (const wide of [false, true]) {
          await checkPacket(compiler, wide, {
            map: '38',
            opcode,
            immediate: '',
            left,
            right,
            result: wide ? xmmResult : mmxResult,
            self: wide ? xmmSelf : mmxSelf,
          });
        }
      },
    }),
  );
  return [
    ...scenarios,
    {
      name: 'SSSE3 PALIGNR consumes its immediate after the full address and crosses both bank boundaries',
      async run() {
        const left = 0x0f0e0d0c0b0a09080706050403020100n,
          right = 0x8f8e8d8c8b8a89888786858483828180n;
        for (const width of [64, 128]) {
          const select = (src: bigint, n: number) =>
            BigInt.asUintN(
              width,
              ((BigInt.asUintN(width, left) << BigInt(width)) | BigInt.asUintN(width, src)) >>
                BigInt(n * 8),
            );
          for (const n of [0, 1, 7, 8, 9, 15, 16, 17, 31, 32, 33, 127, 128, 255]) {
            await checkPacket(compiler, width === 128, {
              map: '3a',
              opcode: '0f',
              immediate: n.toString(16).padStart(2, '0'),
              left,
              right,
              result: select(right, n),
              self: select(left, n),
            });
          }
        }
      },
    },
    {
      name: 'SSSE3 zero-result PALIGNR still reads every source byte, commits debug only after success, and preserves pending MMX state',
      async run() {
        for (const wide of [false, true]) {
          const code = (wide ? '66' : '') + '0f3a0f03ff',
            width = wide ? 16 : 8;
          await using(
            compiler,
            code,
            LONG,
            async (cpu) => {
              const numeric = stack({}, 3),
                vector = banks();
              cpu.setX87State(numeric);
              cpu.setXmmState(vector);
              cpu.setMxcsr(OTHER_MXCSR);
              const target = data + BigInt(width - 1);
              cpu.setDebugState({ addresses: [target, 0n, 0n, 0n], dr7: 0x30401n, dr6: 0n });
              const before = input(0x1000n, { 3: data });
              cpu.setState(before);
              const fail = await cpu.run({ budget: 1n });
              outcome(fail, 'data-fault', 1n, 0n, 1n, 0n, before);
              equal(fail.fault?.failingAddress, target, 'last byte is inaccessible');
              equal(cpu.readDebugState().dr6 & 15n, 0n, 'fault discards tentative debug hit');
              x87(cpu, numeric);
              xmm(cpu, vector);
              equal(cpu.readMxcsr(), OTHER_MXCSR, 'fault preserves MXCSR');
              if (!wide) {
                const pendingState = stack({}, 3, 0x37b, 0x8084);
                cpu.setX87State(pendingState);
                cpu.setState(before);
                const blocked = await cpu.run({ budget: 1n });
                outcome(blocked, 'x87-exception', 1n, 0n, 1n, 0n, before);
                pending(blocked, pendingState, 0x1000n);
                x87(cpu, pendingState);
              }
              cpu.setX87State(numeric);
              cpu.setDebugState({
                addresses: [data + 0x20n + BigInt(width - 1), 0n, 0n, 0n],
                dr7: 0x30401n,
                dr6: 0n,
              });
              const good = input(0x1000n, { 3: data + 0x20n });
              cpu.setState(good);
              const hit = await cpu.run({ budget: 1n });
              equal(hit.reason, 'debug-exception', 'completed source read trap');
              equal(hit.retired, 1n, 'read trap after retirement');
              equal(hit.debugException?.breakpoints, 1, 'read trap slot');
              equal(hit.state.rip, 0x1000n + BigInt(code.length / 2), 'saved next RIP');
              const after = clone(numeric);
              if (wide) vector[0] = 0n;
              else {
                after.registers[0] = 0xffffn << 64n;
                after.tagWord = 0;
                after.statusWord &= ~0x3800;
              }
              x87(cpu, after);
              xmm(cpu, vector);
            },
            {
              data: [
                { base: data, bytes: 'a5'.repeat(width - 1) },
                { base: data + 0x20n, bytes: '5a'.repeat(width) },
              ],
            },
          );
        }
      },
    },
    {
      name: 'SSSE3 reached admission distinguishes MMX from XMM through OSFXSR, EM and TS toggles',
      async run() {
        for (const wide of [false, true]) {
          await usingPaged(compiler, { code: (wide ? '66' : '') + '0f3800c1' }, async (cpu) => {
            for (const [em, ts, osfxsr] of [
              [false, true, false],
              [true, true, true],
              [false, false, false],
              [false, true, true],
            ] as const) {
              cpu.setPagingState({
                ...BANK,
                cr0: BANK.cr0 | (em ? 4n : 0n) | (ts ? 8n : 0n),
                cr4: osfxsr ? BANK.cr4 : BANK.cr4 & ~0x200n,
              });
              const numeric = stack({}, 4),
                vector = banks();
              cpu.setX87State(numeric);
              cpu.setXmmState(vector);
              const before = input(PC, {}, 0x10002n);
              cpu.setState(before);
              const result = await cpu.run({ budget: 1n });
              const reason =
                em || (wide && !osfxsr)
                  ? 'invalid-encoding'
                  : ts
                    ? 'device-not-available'
                    : 'budget';
              equal(result.reason, reason, 'live numeric admission');
              equal(result.retired, reason === 'budget' ? 1n : 0n, 'admission retirement');
              if (reason !== 'budget') {
                equal(result.state.flags, before.flags, 'pre-begin RF');
                x87(cpu, numeric);
                xmm(cpu, vector);
              }
            }
          });
        }
      },
    },
  ];
}
