import { createCpu } from '../../../sdk/src/index.js';
import {
  changed,
  check,
  CODE,
  createFixture,
  equal,
  exit,
  hex,
  initial,
  type Scenario,
} from '../../support/fixture.js';
import { fixture, state, using } from '../alignment-checking/fixture.js';
import { BANK, PC, usingPaged } from '../paging/fixture.js';
import { word } from '../supervisor-access/fixture.js';

const PADDING = [
  '0f1f00',
  '0f1f4000',
  '0f1f440000',
  '660f1f440000',
  '0f1f8000000000',
  '0f1f840000000000',
  '660f1f840000000000',
  '66'.repeat(7) + '0f1f840000000000', // fifteen bytes
  '4c0f1fc0', // /0 register; REX.R does not extend a group selector
  '6467480f1f8408ffffffff', // FS and 32-bit SIB/displacement in long mode
];

export function nopScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'documented alignment NOP lengths and register forms retire once and preserve registers and flags',
      async run() {
        const sequence = PADDING.join('');
        await createFixture(compiler)(sequence, async (cpu) => {
          const input = initial(CODE, { 0: 0x800000000000n, 1: 0xffffeeee00000001n }, 0x8d7n);
          cpu.setState(input);
          const start = cpu.readTimeState().tsc;
          let pc = CODE;
          for (const bytes of PADDING) {
            pc += BigInt(bytes.length / 2);
            exit(await cpu.run({ budget: 1n }), 'budget', 1n, 1n, changed(input, pc));
          }
          equal(cpu.readTimeState().tsc - start, BigInt(PADDING.length), 'one tick per NOP');
        });
      },
    },
    {
      name: 'NOP memory addressing uses neither mapped nor missing CPL3 data and ignores data breakpoints and alignment checking',
      async run() {
        for (const [address, cause] of [
          [0x6000n, 'debug-exception'],
          [0x6001n, 'alignment-check'],
          [0x18001n, 'page-fault'],
        ] as const) {
          const f = fixture(compiler, '0f1f038b03'); // NOP [EBX]; MOV EAX,[EBX]
          f.leaf(0x6000, 0xa007n); // user data present with an initially clear A bit
          f.leaf(0x18000, 0n);
          await using({ createCpu, compiler }, f, async (cpu) => {
            const input = state({ 3: address });
            cpu.setState(input);
            cpu.setDebugState({ addresses: [address, 0n, 0n, 0n], dr6: 0n, dr7: 0x30401n });
            const cr2 = cpu.readPagingState().cr2;
            exit(await cpu.run({ budget: 1n }), 'budget', 1n, 1n, changed(input, 0x1003n));
            equal(cpu.readDebugState().dr6 & 1n, 0n, 'unused memory operand did not match');
            equal(cpu.readPagingState().cr2, cr2, 'no data #PF');
            equal(word(cpu, f.pt + 6 * f.width, f.width), 0xa007n, 'data page A/D unmodified');
            const mov = await cpu.run({ budget: 1n });
            equal(mov.reason, cause, 'real read exposes the selected boundary');
          });
        }
        const f = fixture(compiler, '670f1f800100b834120000'); // 16-bit [BX+SI+disp16] then MOV EAX,1234h
        await using({ createCpu, compiler }, f, async (cpu) => {
          cpu.setState(state());
          const pair = await cpu.run({ budget: 2n });
          equal(pair.reason, 'budget', 'address size selected the two-byte displacement');
          equal(pair.state.rip, 0x100bn, 'exact next instruction length');
          equal(pair.state.registers[0]! & 0xffffffffn, 0x1234n, 'successor executes intact');
        });
      },
    },
    {
      name: 'multibyte NOP requires only its encoded bytes, including LOCK, and length rejects byte sixteen before fetch',
      async run() {
        for (const [tail, outcome] of [
          ['0f1f', 'page-fault'], // missing ModR/M
          ['0f1f84', 'page-fault'], // missing SIB
          ['0f1f8400000000', 'page-fault'], // final displacement byte
          ['f00f1f8400000000', 'page-fault'], // LOCK still needs the selected address tail
          ['f00f1fc0', 'invalid-encoding'], // complete illegal LOCK NOP -> #UD
          ['66'.repeat(13) + '0f1f', 'invalid-encoding'], // ModR/M would be byte sixteen -> #GP
          [PADDING[7]!, 'budget'], // exact fifteen bytes ending at page boundary
        ] as const) {
          await usingPaged(
            compiler,
            { bank: { ...BANK, cr2: 0xabcden, efer: 0xd00n } },
            async (cpu) => {
              const start = PC + 0x1000n - BigInt(tail.length / 2);
              cpu.writePhysicalMemory(0xa000n - BigInt(tail.length / 2), hex(tail));
              cpu.setState(initial(start));
              cpu.setDebugState({ addresses: [start, 0n, 0n, 0n], dr6: 0n, dr7: 0x401n });
              const debug = await cpu.run({ budget: 1n });
              equal(
                debug.reason,
                'debug-exception',
                'execute breakpoint precedes fetching the tail',
              );
              equal(cpu.readPagingState().cr2, 0xabcden, 'breakpoint preserves CR2');
              cpu.setDebugState({ addresses: [0n, 0n, 0n, 0n], dr6: 0n, dr7: 0x400n });
              const result = await cpu.run({ budget: 1n });
              equal(result.reason, outcome, 'reached NOP/decode boundary');
              equal(
                result.retired,
                outcome === 'budget' ? 1n : 0n,
                'only complete valid form retires',
              );
              if (outcome === 'page-fault') {
                equal(result.pageFault?.address, PC + 0x1000n, 'first missing instruction byte');
                equal(result.pageFault?.errorCode, 16, 'supervisor fetch');
                equal(result.state.rip, start, 'incomplete encoding does not advance RIP');
              } else {
                equal(cpu.readPagingState().cr2, 0xabcden, 'no fetch beyond encoded length limit');
                if (outcome === 'invalid-encoding') {
                  const staged = await cpu.run({ budget: 1n, eventDelivery: { budget: 0n } });
                  check(staged.state.event.kind === 'pending', 'architectural decode fault');
                  const gp = tail.length === 30;
                  equal(staged.state.event.exception.vector, gp ? 13 : 6, 'length #GP or LOCK #UD');
                  equal(
                    staged.state.event.exception.errorCode,
                    gp ? 0 : undefined,
                    'decode error code',
                  );
                }
              }
            },
          );
        }
      },
    },
  ];
}
