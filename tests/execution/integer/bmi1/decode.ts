import { check, equal, exit, hex, initial, type Scenario } from '../../../support/fixture.js';
import { same } from '../../far-control/fixture.js';
import { context, LONG, using } from '../../modes/fixture.js';
import { BANK, PC, usingPaged } from '../../paging/fixture.js';

export function decodeScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'BMI1 instruction fetch completes selected address tails before invalid encodings, within fifteen bytes',
      async run() {
        // ANDN with ModR/M, SIB and disp32. Even an illegal selected form needs its last byte.
        const addressTail = 'c4e278f28400010203';
        for (const tail of [
          'f0' + addressTail,
          '48' + addressTail,
          'c4e27cf28400010203',
          ...[1, 2, 3, 4, 5, 6, 7, 8, 9].map((length) => addressTail.slice(0, length * 2)),
        ]) {
          await usingPaged(compiler, { bank: { ...BANK, efer: 0xd00n } }, async (cpu) => {
            const missing = PC + 0x1000n;
            const start = missing - BigInt(tail.length / 2);
            cpu.writePhysicalMemory(0xa000n - BigInt(tail.length / 2), hex(tail));
            const before = initial(start);
            cpu.setState(before);
            const result = await cpu.run({ budget: 1n });
            equal(result.reason, 'page-fault', `missing encoding byte after ${tail}`);
            exit(result, 'page-fault', 1n, 0n, before);
            equal(result.pageFault?.address, missing, 'first missing instruction byte');
            equal(result.pageFault?.errorCode, 16, 'supervisor fetch');
          });
        }
        for (const [tail, overlong] of [
          ['64'.repeat(10) + 'c4e278f3c9', false], // FS ... BLSR EAX,ECX fits exactly.
          ['64'.repeat(11) + 'c4e278f3', true], // ModR/M would be byte sixteen.
        ] as const) {
          await usingPaged(
            compiler,
            { bank: { ...BANK, cr2: 0x5678n, efer: 0xd00n } },
            async (cpu) => {
              cpu.writePhysicalMemory(0x9ff1n, hex(tail));
              const start = PC + 0xff1n;
              cpu.setState(initial(start, { 1: 0x15n }));
              const result = await cpu.run({ budget: 1n, eventDelivery: { budget: 0n } });
              equal(cpu.readPagingState().cr2, 0x5678n, 'length limit never fetches byte sixteen');
              if (overlong) {
                check(result.state.event.kind === 'pending', 'overlong instruction stages a fault');
                equal(result.state.event.exception.vector, 13, '#GP');
                equal(result.state.event.exception.errorCode, 0, 'length violation');
                equal(result.retired, 0n, 'no overlong retirement');
              } else {
                equal(result.reason, 'budget', 'valid fifteen-byte instruction');
                equal(result.retired, 1n, 'one BLSR');
                equal(result.state.registers[0], 0x14n, 'last byte selects ECX');
                equal(result.state.rip, PC + 0x1000n, 'exact fifteen-byte length');
              }
            },
          );
        }
      },
    },
    {
      name: 'BMI1 samples aliased controls before writeback and restricts VEX register extension to long mode',
      async run() {
        for (const [code, which, expected] of [
          ['c442b0f2cd', 9, 0x1122334455667700n], // ANDN R9,R9,R13
          ['c442b0f7cd', 9, 0x77n], // BEXTR R9,R13,R9
          ['c44290f7ed', 13, 0x0n], // BEXTR R13,R13,R13 (start=ff)
        ] as const) {
          await using(compiler, code, LONG, async (cpu) => {
            const input = initial(0x1000n, {
              9: code === 'c442b0f2cd' ? 255n : 0x0808n,
              13: 0x11223344556677ffn,
            });
            cpu.setState(input);
            const result = await cpu.run({ budget: 1n });
            equal(result.reason, 'budget', 'aliased operation completes');
            const bank = [...input.registers];
            bank[which] = expected;
            same(result.state.registers, bank, 'only the aliased destination changes');
          });
        }
        for (const mode of [LONG, context('protected'), context('compatibility')]) {
          const isLong = mode.mode === 'long64';
          for (const [code, destination, value] of [
            ['c4e238f3c9', isLong ? 8 : 0, 0x14n], // encoded vvvv extends destination
            ['c4c278f3c9', 0, isLong ? 0x30n : 0x14n], // encoded B extends r/m source
          ] as const) {
            await using(compiler, code, mode, async (cpu) => {
              const before = initial(0x1000n, { 1: 0x15n, 9: 0x38n });
              cpu.setState(before);
              const result = await cpu.run({ budget: 1n });
              equal(result.reason, 'budget', 'BLSR GPR encoding');
              const bank = [...before.registers];
              bank[destination] = isLong ? value : (bank[destination]! & ~0xffffffffn) | value;
              same(result.state.registers, bank, `VEX extension in ${mode.mode}`);
            });
          }
        }
      },
    },
  ];
}
