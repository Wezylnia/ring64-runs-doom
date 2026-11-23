import { equal, memory } from '../../../../support/fixture.js';
import { simdFault } from '../../floating/fixture.js';
import {
  DATA,
  PC,
  createCpu,
  fixture,
  hex,
  key,
  pageFault,
  pair,
  same,
  state,
  using as usingPhysical,
} from '../../../memory/protection-keys/fixture.js';
import {
  LONG,
  banks,
  context,
  input,
  profiles,
  using,
  vector,
  xmm,
  type Scenario,
} from '../fixture.js';

export function sse3Scenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'SSE3 guest selects the CPUID path and computes both outputs of a complex butterfly',
      async run() {
        const dispatch = 'b8010000000fa2f7c1010000007422';
        // Reorder [a,b,c,d] to [a,c,b,d], duplicate both inputs, then compute
        // the real and imaginary parts of (a+ib) ± (c+id) in four lanes.
        const butterfly = '0f10060fc6c0d8f30f12c8f30f16d0f20fd0ca0fc6c98d0f110f';
        const paths = '41ba01000000eb0641ba00000000';
        const end = PC + BigInt((dispatch + butterfly + paths).length / 2);
        await using(
          compiler,
          dispatch + butterfly + paths + '90',
          LONG,
          async (cpu) => {
            cpu.setState(input(PC, { 6: DATA, 7: DATA + 0x100n }));
            cpu.setMxcsr(0x1f80);
            const result = await cpu.run({ budget: 32n, stop: end });
            equal(result.reason, 'stop', 'butterfly reached join');
            equal(result.state.registers[10], 1n, 'guest took the SSE3 path');
            equal(result.retired, 13n, 'only the selected branch retired');
            memory(cpu, DATA + 0x100n, '000000400000c0bf0000a040000020c0');
            equal(cpu.readMxcsr(), 0x1f80, 'exact finite operations');
          },
          {
            data: [
              { base: DATA, bytes: '00006040000000c00000c0bf0000003f', write: false },
              { base: DATA + 0x100n, bytes: 'a5'.repeat(16) },
            ],
          },
        );
      },
    },
    {
      name: 'HADDPS arbitrates all lanes before publishing the dirty destination and retries at its middle entry',
      async run() {
        const source = banks();
        source[2] = vector([0x7f7fffffn, 0x7f7fffffn, 0x3f800000n, 0x3f800000n], 32);
        source[1] = vector([0x7f800042n, 0n, 0x3f800000n, 0x3f800000n], 32);
        await using(compiler, '0f28c2f20f7cc1', LONG, async (cpu) => {
          cpu.setState(input());
          cpu.setXmmState(source);
          cpu.setMxcsr(0x1b00);
          const first = await cpu.run({ budget: 2n });
          equal(first.reason, 'simd-exception', 'unmasked SNaN');
          equal(first.retired, 1n, 'only predecessor retired');
          equal(first.state.rip, PC + 3n, 'faulting middle entry');
          simdFault(first, 0x1b00, 0x1b01, 1, 1, 'pre-computation', PC + 3n);
          source[0] = source[2]!;
          xmm(cpu, source);

          cpu.setMxcsr(0x1f80);
          const retry = await cpu.run({ budget: 1n });
          equal(retry.reason, 'budget', 'masked HADDPS retires');
          equal(retry.retired, 1n, 'one retry');
          equal(retry.resources.compilations, 0, 'retained entry');
          source[0] = vector([0x7f800000n, 0x40000000n, 0x7fc00042n, 0x40000000n], 32);
          xmm(cpu, source);
          equal(cpu.readMxcsr(), 0x1fa9, 'masked invalid, overflow and precision');
        });
      },
    },
    {
      name: 'HADDPD reads the FS-selected m128 through both legacy address sizes',
      async run() {
        for (const { mode, code32, stack32 } of profiles) {
          if (mode === 'long64' || code32 !== stack32) continue;
          const execution = context(mode, code32, stack32);
          execution.segments.fs.base = DATA - 0x1000n;
          for (const override of [false, true]) {
            const addr16 = code32 === override;
            const code = '6466' + (override ? '67' : '') + '0f7c' + (addr16 ? '07' : '00');
            await using(
              compiler,
              code,
              execution,
              async (cpu) => {
                cpu.setState(
                  input(PC, { [addr16 ? 3 : 0]: addr16 ? 0x22331000n : 0x112200001000n }),
                );
                const expected = banks();
                expected[0] = vector([0x3ff0000000000000n, 0x4000000000000000n], 64);
                cpu.setXmmState(expected);
                cpu.setMxcsr(0x1f80);
                const r = await cpu.run({ budget: 1n });
                equal(r.reason, 'budget', 'memory HADDPD');
                equal(r.state.rip, PC + BigInt(code.length / 2), 'full encoding');
                expected[0] = vector([0x4008000000000000n, 0xbff0000000000000n], 64);
                xmm(cpu, expected);
                equal(cpu.readMxcsr(), 0x1f80, 'exact');
              },
              { data: [{ base: DATA, bytes: '000000000000084000000000000010c0', write: false }] },
            );
          }
        }
      },
    },
    {
      name: 'SSE3 MOVDDUP stops at eight bytes and LDDQU observes a denied second page before its read watchpoint',
      async run() {
        const code = 'f20f1203f20ff00b';
        const image = fixture(compiler, code, { cpl: 3 });
        image.leaf(Number(DATA), 0xa067n);
        image.leaf(Number(DATA + 0x1000n), key(3) | 0x10067n);
        image.bytes.set(hex('420000000000f07f'), 0xaff8);
        image.bytes.set(hex('8000000000000080'), 0x10000);
        await usingPhysical({ createCpu, compiler }, image, async (cpu) => {
          const before = state();
          before.registers[3] = DATA + 0xff8n;
          const expected = banks();
          cpu.setXmmState(expected);
          cpu.setState(before);
          cpu.setMxcsr(0x1f00);
          cpu.setDebugState({ addresses: [DATA + 0x1007n, 0n, 0n, 0n], dr6: 0n, dr7: 0x30401n });
          cpu.setProtectionKeysState({ pkru: pair(3, 1), pkrs: 0 });
          const first = await cpu.run({ budget: 2n });
          pageFault(first, DATA + 0x1000n, 'read', 0x25, PC + 4n);
          equal(first.retired, 1n, 'm64 ended on first page');
          expected[0] = vector([0x7ff0000000000042n, 0x7ff0000000000042n], 64);
          xmm(cpu, expected);
          equal(cpu.readMxcsr(), 0x1f00, 'MOVDDUP preserved signaling NaN');
          same(cpu.readDebugState().dr6 & 1n, 0n, 'discard denied read watch');
          cpu.setProtectionKeysState({ pkru: 0, pkrs: 0 });
          const retry = await cpu.run({ budget: 1n });
          equal(retry.reason, 'debug-exception', 'successful LDDQU reports read');
          equal(retry.state.rip, PC + 8n, 'read trap has successor RIP');
          equal(retry.retired, 1n, 'LDDQU retired once');
          equal(retry.resources.compilations, 0, 'same middle entry after PKRU repair');
          expected[1] = vector([0x7ff0000000000042n, 0x8000000000000080n], 64);
          xmm(cpu, expected);
          equal(cpu.readMxcsr(), 0x1f00, 'LDDQU preserved control state');
        });
      },
    },
  ];
}
