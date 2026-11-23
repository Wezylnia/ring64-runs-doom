import { usesMemory64 } from '../../../../../sdk/src/wasm/core.js';
import { observeArtifactImports } from '../../../../support/artifact-imports.js';
import { context } from '../../../processor/modes/fixture.js';
import { BANK, equal, initial, little, same, usingPaged, word, type Scenario } from '../fixture.js';

export function scalarReplayScenarios(compiler: WebAssembly.Module): Scenario[] {
  const privateSpans = !usesMemory64(compiler);
  return [
    {
      name: 'paged dense byte replay preserves the prefix of a faulting lap',
      async run() {
        const head = 0x400000n;
        const missing = head + 0x2000n;
        // Four consecutive MOVZX loads, a four-byte stride, CMP and JNE.
        const code = '0fb6040e0fb65c0e010fb6540e020fb67c0e034883c1044c39c175e4';
        await usingPaged(
          compiler,
          {
            code,
            words: [[0x4008n, 0xa007n]],
            spans: [{ physical: 0xa000n, bytes: Uint8Array.from({ length: 4096 }, (_, i) => i) }],
          },
          async (cpu) => {
            cpu.setState(initial(head, { 1: 0n, 6: missing - 39n, 8: 64n }, 2n));
            const result = await cpu.run({ stop: head + 28n, budget: 120n });
            equal(result.reason, 'page-fault', 'the checked read reports the next missing leaf');
            equal(result.retired, 66n, 'nine complete laps and three reads retire');
            equal(result.state.rip, head + 14n, 'the fourth read remains the next instruction');
            equal(result.state.registers[1], 36n, 'the partial lap leaves the index intact');
            same(
              [0, 3, 2, 7].map((index) => result.state.registers[index]),
              [253n, 254n, 255n, 252n],
              'earlier reads commit and the faulting destination keeps its prior value',
            );
            equal(result.state.flags & 0x8d7n, 0x87n, 'the previous compare flags survive');
            equal(cpu.readPagingState().cr2, missing, 'CR2 records the original read address');
            equal(word(cpu, 0x4008n), 0xa027n, 'the earlier reads mark their leaf accessed');
          },
        );
      },
    },
    {
      name: 'paged 32-bit query suffix replays a refused stack read at its head once',
      async run() {
        const head = 0x400000n;
        const data = 0x401000n;
        const stack = 0x402000n;
        const unaligned = stack + 1n;
        // MOV [ESP],EAX; suffix: CMP EAX,[ESP]; MOV EDX,[ESI]; JMP inner;
        // rotate: CMP EBX,ECX; JAE head;
        // inner: CMP [EDI],EDX; JAE rotate; INC EBX; CMP EBX,ECX; JB inner; JMP head.
        const code = '8904243b04248b16eb0439cb73f2391773f84339cb72f7ebe7';
        let refusals = 0;
        const restore = observeArtifactImports((env) => {
          const read = env.paged_loop_read_dword;
          if (typeof read === 'function' && typeof env.paged_write_loop_guard32 === 'function') {
            env.paged_loop_read_dword = (address: bigint) => {
              const value = read(address);
              if (address === unaligned) {
                equal(++refusals, 1, 'reject a retry of the same guarded stack operand');
                equal(value, -1n, 'the unaligned operand must use the checked path');
              }
              return value;
            };
          }
        });
        try {
          await usingPaged(
            compiler,
            {
              code,
              context: context('compatibility'),
              words: [
                [0x4008n, 0xa027n],
                [0x4010n, 0xb067n],
              ],
              spans: [
                { physical: 0xa000n, bytes: little([0x13579bdfn], 4) },
                { physical: 0xb000n, bytes: new Uint8Array(8) },
              ],
            },
            async (cpu) => {
              const stop = head + 14n;
              const start = (esp: bigint) =>
                initial(head, { 0: 7n, 2: 0n, 4: esp, 6: data, 7: data }, 2n);
              cpu.setState(start(stack));
              equal((await cpu.run({ stop, budget: 16n })).reason, 'stop', 'warm common fetch');

              cpu.setState(start(unaligned));
              const result = await cpu.run({ stop, budget: 16n });
              equal(refusals, 1, 'the admitted suffix tries its first operand');
              equal(result.reason, 'stop', 'checked replay reaches the inner entry');
              equal(result.retired, 4n, 'the spill, comparison, load and jump retire once each');
              equal(result.state.rip, stop, 'the original jump target is preserved');
              equal(result.state.flags, 0x46n, 'the checked comparison sees the written value');
              equal(result.state.registers[2], 0x13579bdfn, 'the successor load completes');
              same(
                [...cpu.readPhysicalMemory(0xb001n, 4)],
                [7, 0, 0, 0],
                'the original checked spill completes',
              );
            },
          );
        } finally {
          restore();
        }
      },
    },
    {
      name: 'cold straight paged dword reads reenter only after successful checked replay',
      async run() {
        const head = 0x400000n;
        const data = 0x401000n;
        // Four MOV EAX,[RDI+RAX*4]; DEC ECX; JNZ head; HLT.
        const code = '8b04878b04878b04878b0487ffc975f0f4';
        const calls = { refused: 0, ready: 0, guarded: 0 };
        const restore = observeArtifactImports((env) => {
          const read = env.paged_loop_walk_dword;
          const ready = env.paged_loop_read_ready;
          if (typeof read === 'function') {
            env.paged_loop_walk_dword = (address: bigint) => {
              const result = read(address);
              if (result === -1n) calls.refused++;
              else calls.guarded++;
              return result;
            };
          }
          if (typeof ready === 'function') {
            env.paged_loop_read_ready = (address: bigint, width: number) => {
              const result = ready(address, width);
              if (result === 1) calls.ready++;
              return result;
            };
          }
        });
        try {
          for (const mode of ['cold', 'absent', 'denied', 'stop', 'single-step'] as const) {
            Object.assign(calls, { refused: 0, ready: 0, guarded: 0 });
            await usingPaged(
              compiler,
              {
                code,
                words: [[0x4008n, mode === 'absent' ? 0n : 0xa007n]],
                spans: [
                  {
                    physical: 0xa000n,
                    bytes: little([1n, 2n, 3n, 0n], 4),
                    read: mode !== 'denied',
                  },
                ],
              },
              async (cpu) => {
                cpu.setState(
                  initial(head, { 0: 0n, 1: 24n, 7: data }, mode === 'single-step' ? 0x102n : 2n),
                );
                const stop = head + (mode === 'stop' ? 3n : 16n);
                const result = await cpu.run({ stop, budget: 200n });
                if (mode === 'cold') {
                  equal(calls.refused, 1, 'the first unaccessed leaf uses exact replay');
                  equal(calls.ready, Number(privateSpans), 'private span re-entry availability');
                  equal(
                    calls.guarded > 0,
                    privateSpans,
                    'later dword reads use an admitted private span',
                  );
                  equal(result.reason, 'stop', mode);
                  equal(result.state.rip, stop, mode);
                  equal(result.retired, 144n, mode);
                  equal(result.state.registers[0], 0n, 'complete pointer-chain laps');
                } else {
                  equal(calls.ready, 0, mode + ' does not publish re-entry');
                  equal(calls.guarded, 0, mode + ' does not run a guarded read');
                  if (mode === 'absent' || mode === 'denied') {
                    equal(
                      result.reason,
                      mode === 'absent' ? 'page-fault' : 'physical-memory-fault',
                      mode,
                    );
                    equal(result.state.rip, head, mode);
                    equal(result.retired, 0n, mode);
                    equal(result.state.registers[0], 0n, 'the failing MOV is unstarted');
                    if (mode === 'absent')
                      equal(cpu.readPagingState().cr2, data, 'original guest address');
                    else
                      equal(
                        result.physicalMemoryFault?.phase,
                        'final-access',
                        'original backing refusal',
                      );
                  } else {
                    equal(result.reason, mode === 'stop' ? 'stop' : 'debug-exception', mode);
                    equal(result.state.rip, head + 3n, mode);
                    equal(result.retired, 1n, mode);
                    equal(result.state.registers[0], 1n, 'exact first read result');
                  }
                }
                equal(
                  word(cpu, 0x4008n),
                  mode === 'absent' ? 0n : 0xa027n,
                  'ordinary read A/D semantics',
                );
              },
            );
          }
        } finally {
          restore();
        }
      },
    },
    {
      name: 'paged scalar replay qualifies only a successful ordinary checked read',
      async run() {
        const head = 0x400000n;
        const data = head + 0x1000n;
        const oldCr2 = 0x1234n;
        const code = '83c3018b1685d2740190ffc975f2';
        for (const kind of ['aligned', 'unaligned', 'split-ram', 'fault', 'partial'] as const) {
          const address = data + (kind === 'unaligned' ? 1n : 0n);
          await usingPaged(
            compiler,
            {
              code,
              bank: { ...BANK, cr2: oldCr2 },
              ...(kind === 'fault' ? {} : { words: [[0x4008n, 0xa007n]] }),
              spans:
                kind === 'split-ram'
                  ? [
                      { physical: 0xa000n, bytes: Uint8Array.of(0x78, 0x56) },
                      { physical: 0xa002n, bytes: Uint8Array.of(0x34, 0x12) },
                    ]
                  : [{ physical: 0xa000n, bytes: little([0x12345678n, 0n], 4) }],
            },
            async (cpu) => {
              cpu.setState(initial(head, { 1: 20n, 2: 0x4242n, 3: 0x7fffffffn, 6: address }, 2n));
              const result = await cpu.run({
                budget: kind === 'partial' ? 2n : 160n,
                stop: head + 14n,
              });
              if (kind === 'fault') {
                equal(result.reason, 'page-fault', 'original checked access reports PF');
                equal(result.retired, 1n, 'the preceding addition alone retires');
                equal(result.state.rip, head + 3n, 'the failed read remains the next instruction');
                equal(result.state.flags & 0x8d7n, 0x896n, 'the preceding addition flags survive');
                equal(result.state.registers[2], 0x4242n, 'the destination is unchanged');
                equal(cpu.readPagingState().cr2, address, 'CR2 comes from the checked exception');
                return;
              }
              equal(result.reason, kind === 'partial' ? 'budget' : 'stop', 'ordinary run boundary');
              equal(
                result.retired,
                kind === 'partial' ? 2n : 140n,
                'each instruction retires once',
              );
              equal(result.state.rip, head + (kind === 'partial' ? 5n : 14n), 'exact resume PC');
              equal(
                result.state.registers[2],
                kind === 'unaligned' ? 0x123456n : 0x12345678n,
                'the checked or retained value commits once',
              );
              equal(word(cpu, 0x4008n), 0xa027n, 'only the ordinary read marks the leaf accessed');
              equal(cpu.readPagingState().cr2, oldCr2, 'successful reads leave prior CR2');
            },
          );
        }
      },
    },
  ];
}
