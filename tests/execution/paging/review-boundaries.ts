import { observeArtifactImports } from '../../support/artifact-imports.js';
import { Core } from '../../../sdk/src/wasm/core.js';
import { snapshot } from '../far-control/fixture.js';
import { context } from '../modes/fixture.js';
import {
  BANK,
  check,
  equal,
  hex,
  initial,
  little,
  same,
  usingPaged,
  word,
  type Scenario,
} from './fixture.js';

// MOV EAX,[ESP+4]; CMP EAX,[ESP+8]; JBE spill; INC EAX; MOV [ESP+4],EAX;
// MOVZX EDX,[EDI]; ADD EDX,EAX; MOV [ESI],DL; INC ESI; INC EDI; DEC ECX; JNZ head.
const PAGED_STACK_SPILL_LOOP = '8b4424043b442408760140894424040fb61701c2881646474975e5';
// MOVZX EAX,[RDI]; CMP AL,40h; JBE store; INC AL; MOV [RSI],AL;
// INC RDI; INC RSI; DEC ECX; JNZ head.
const PAGED_BYTE_STORE_LOOP = '0fb6073c407602fec0880648ffc748ffc6ffc975eb';

export function pagingBoundaryReviewScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'paged guarded prefixes preserve both conditional edges and precise pre-branch exits',
      async run() {
        const head = 0x400000n;
        const branch = head + 4n;
        const end = head + 11n;
        const data = 0x401000n;
        // INC ECX; CMP ECX,[RSI]; JB end; MOV ECX,3; end: HLT.
        const code = 'ffc13b0e7205b903000000f4';
        const guards: number[] = [];
        const restore = observeArtifactImports((env) => {
          const guard = env.paged_loop_guard;
          if (typeof guard === 'function') {
            env.paged_loop_guard = (pc: bigint, bytes: number, flags: bigint, shadow: number) => {
              const status = guard(pc, bytes, flags, shadow);
              if (pc === head && bytes === 6) guards.push(status);
              return status;
            };
          }
        });
        try {
          await usingPaged(
            compiler,
            {
              code,
              words: [[0x4008n, 0xa007n]],
              spans: [{ physical: 0xa000n, bytes: little([2n], 4) }],
            },
            async (cpu) => {
              const prepare = (count: bigint, operand = data) => {
                cpu.setState(initial(head, { 1: count, 6: operand }, 2n));
                guards.length = 0;
              };
              prepare(0n);
              equal(
                (await cpu.run({ stop: end, budget: 12n })).reason,
                'stop',
                'cold compare fills translation',
              );
              prepare(0n);
              const taken = await cpu.run({ stop: end, budget: 12n });
              equal(taken.reason, 'stop', 'the taken edge reaches its stop');
              equal(taken.retired, 3n, 'the taken edge skips fallthrough MOV');
              equal(taken.state.registers[1], 1n, 'the taken edge carries the increment');
              same(guards, [1], 'the branch retires in the clone');

              prepare(1n);
              const fallthrough = await cpu.run({ stop: end, budget: 12n });
              equal(fallthrough.reason, 'stop', 'the untaken edge reaches its stop');
              equal(fallthrough.retired, 4n, 'the fallthrough MOV also retires');
              equal(fallthrough.state.registers[1], 3n, 'fallthrough uses its ordinary label');
              same(guards, [1], 'the false edge leaves the same clone');

              prepare(0n);
              const stopped = await cpu.run({ stop: branch, budget: 12n });
              equal(stopped.reason, 'stop', 'the branch remains unstarted at an interior stop');
              equal(stopped.retired, 2n, 'only the compare and increment retire');
              same(guards, [], 'the interior stop refuses the full clone');

              prepare(0n, data + 0x1000n);
              const fault = await cpu.run({ stop: end, budget: 12n });
              equal(fault.reason, 'page-fault', 'the refused compare reports its fault');
              equal(fault.state.rip, head + 2n, 'the compare goes back to its original label');
              equal(fault.retired, 1n, 'the increment executes once');
              equal(fault.state.registers[1], 1n, 'the committed count is preserved');
              same(guards, [1], 'only the original compare replays');
            },
          );
        } finally {
          restore();
        }
      },
    },
    {
      name: 'paged mixed prefixes decline unadmitted scalar stores at the original write',
      async run() {
        const head = 0x400000n;
        const end = head + 13n;
        const data = 0x401000n;
        // MOV EDX,[RDI]; INC ECX; MOV [RSI],EAX; XOR EAX,5; MOV EBX,[RDI]; INC EDX; HLT.
        const code = '8b17ffc1890683f0058b1fffc2f4';
        const writes: Array<[bigint, number]> = [];
        const restore = observeArtifactImports((env) => {
          const write = env.paged_loop_write_dword;
          if (typeof write === 'function') {
            env.paged_loop_write_dword = (address: bigint, value: number) => {
              const result = write(address, value);
              writes.push([address, result]);
              return result;
            };
          }
        });
        try {
          await usingPaged(
            compiler,
            {
              code,
              words: [[0x4008n, 0xa007n]],
              spans: [{ physical: 0xa000n, bytes: little([19n, 0n], 4) }],
            },
            async (cpu) => {
              const prepare = (destination: bigint, source = 4n) => {
                cpu.setState(
                  initial(head, { 0: source, 1: 7n, 2: 23n, 6: destination, 7: data }, 2n),
                );
                writes.length = 0;
              };
              prepare(data + 4n);
              equal((await cpu.run({ stop: end, budget: 8n })).reason, 'stop', 'cold checked fill');
              prepare(data + 4n);
              const warm = await cpu.run({ stop: end, budget: 8n });
              equal(warm.reason, 'stop', 'the warmed mixed prefix completes');
              equal(warm.retired, 6n, 'the store and its neighbors retire');
              equal(word(cpu, 0xa000n) >> 32n, 4n, 'the retained store commits its source dword');
              same(writes, [[data + 4n, 1]], 'the warmed writable operand takes the clone');

              const missing = 0x402000n;
              prepare(missing);
              const fault = await cpu.run({ stop: end, budget: 8n });
              equal(fault.reason, 'page-fault', 'the ordinary path publishes the fault');
              equal(fault.pageFault?.address, missing, 'the write faults at its original address');
              equal(fault.state.rip, head + 4n, 'the refused write stays unstarted');
              equal(fault.retired, 2n, 'the leading load and increment retire');
              equal(fault.state.registers[1], 8n, 'the increment executes once');
              equal(fault.state.registers[2], 19n, 'the load commits before the write fault');
              equal(fault.state.registers[0], 4n, 'the post-write XOR remains unstarted');
              same(writes, [[missing, 0]], 'the unbacked write refuses without a store');

              const codeAlias = head + 8n;
              // The aligned dword changes XOR's immediate from 5 to 6.
              const changed = 0xff1f8b06n;
              prepare(codeAlias, changed);
              const watched = await cpu.run({ stop: end, budget: 8n });
              equal(watched.reason, 'stop', 'self-modifying code uses the ordinary store');
              equal(watched.retired, 6n, 'the write retires once before running the new owner');
              equal(watched.state.registers[0], changed ^ 6n, 'the changed XOR executes');
              equal(watched.state.registers[1], 8n, 'the earlier increment is not replayed');
              same(writes, [[codeAlias, 0]], 'the clone never writes executable backing');
              equal(
                word(cpu, 0x9008n) & 0xffffffffn,
                changed,
                'the ordinary store commits code bytes',
              );
            },
          );
        } finally {
          restore();
        }
      },
    },
    {
      name: 'paged mixed prefixes resume the checked scalar read without repeating earlier effects',
      async run() {
        const head = 0x400000n;
        const secondRead = head + 7n;
        const end = head + 11n;
        const data = 0x401000n;
        const missing = 0x402000n;
        // INC ECX; MOV EAX,[RDI]; XOR EAX,5; ADD EAX,[RSI]; INC EDX; HLT.
        const code = 'ffc18b0783f0050306ffc2f4';
        const reads: Array<[bigint, boolean]> = [];
        const restore = observeArtifactImports((env) => {
          const read = env.paged_loop_read_span;
          if (typeof read === 'function') {
            env.paged_loop_read_span = (address: bigint, width: number, segment: number) => {
              const pointer = read(address, width, segment);
              reads.push([address, pointer !== 0]);
              return pointer;
            };
          }
        });
        try {
          await usingPaged(
            compiler,
            {
              code,
              words: [[0x4008n, 0xa007n]],
              spans: [{ physical: 0xa000n, bytes: little([2n, 11n], 4) }],
            },
            async (cpu) => {
              const start = initial(head, { 1: 4n, 2: 23n, 6: data + 4n, 7: data }, 2n);
              cpu.setState(start);
              const cold = await cpu.run({ stop: end, budget: 8n });
              equal(cold.reason, 'stop', 'the checked read fills the data translation');
              cpu.setState(start);
              reads.length = 0;
              const warm = await cpu.run({ stop: end, budget: 8n });
              equal(warm.reason, 'stop', 'the guarded prefix reaches its successor');
              equal(warm.retired, 5n, 'both loads and three register operations retire');
              equal(
                warm.state.registers[0],
                18n,
                'the second operand uses the updated accumulator',
              );
              same(
                reads,
                [
                  [data, true],
                  [data + 4n, true],
                ],
                'both warmed reads take the guard',
              );

              cpu.setState(initial(head, { 1: 4n, 2: 23n, 6: missing, 7: data }, 2n));
              reads.length = 0;
              const fault = await cpu.run({ stop: end, budget: 8n });
              equal(fault.reason, 'page-fault', 'the observed read publishes the paging exception');
              equal(fault.state.rip, secondRead, 'the refused instruction remains unstarted');
              equal(fault.pageFault?.address, missing, 'the original operand address faults');
              equal(fault.retired, 3n, 'the first three effects retire once');
              equal(fault.state.registers[0], 7n, 'the successful load and XOR remain committed');
              equal(fault.state.registers[1], 5n, 'the guarded prefix does not repeat its INC');
              equal(fault.state.registers[2], 23n, 'the suffix does not run');
              equal(fault.state.flags, 2n, 'the fault preserves the preceding XOR flags');
              same(
                reads,
                [
                  [data, true],
                  [missing, false],
                ],
                'only the original faulting slot replays',
              );

              cpu.setState(start);
              reads.length = 0;
              const stop = await cpu.run({ stop: secondRead, budget: 8n });
              equal(stop.reason, 'stop', 'an interior stop wins over the second read');
              equal(stop.retired, 3n, 'the stop remains precise');
              same(reads, [], 'an interior stop refuses the mixed clone');

              cpu.setState(start);
              const partial = await cpu.run({ stop: end, budget: 3n });
              equal(partial.reason, 'budget', 'a short budget takes the checked prefix');
              same(partial.state, stop.state, 'the same prefix commits at the budget boundary');
              same(reads, [], 'a short budget skips guarded operands');
            },
          );
        } finally {
          restore();
        }
      },
    },
    {
      name: 'paged straight register prefixes preserve precise stops, debug, and code replacement',
      async run() {
        // MOV EAX,EDI; INC EAX; SHL EAX,1; XOR EAX,5; AND EAX,255;
        // MOV [RSI],EAX; NOP.
        const head = 0x400000n;
        const end = head + 14n;
        const code = '89f8ffc0d1e083f00525ff000000890690';
        const guards: number[] = [];
        const restore = observeArtifactImports((env) => {
          const guard = env.paged_loop_guard;
          if (typeof guard === 'function') {
            env.paged_loop_guard = (pc: bigint, size: number, flags: bigint, shadow: number) => {
              const status = guard(pc, size, flags, shadow);
              if (pc === head && size === 14) guards.push(status);
              return status;
            };
          }
        });
        try {
          await usingPaged(compiler, { code }, async (cpu) => {
            const start = initial(head, { 0: 42n, 6: 0x401000n, 7: 3n }, 2n);
            cpu.setState(start);
            const warm = await cpu.run({ stop: end, budget: 6n });
            equal(warm.reason, 'stop', 'the complete prefix stops at the following store');
            equal(warm.retired, 5n, 'the full prefix retires');
            equal(warm.state.registers[0], 13n, 'the register chain commits');

            cpu.setState(start);
            const interior = await cpu.run({ stop: head + 4n, budget: 6n });
            equal(interior.reason, 'stop', 'an interior stop wins');
            equal(interior.retired, 2n, 'only instructions preceding the stop retire');
            equal(interior.state.registers[0], 4n, 'the shift has not executed');

            cpu.setState(start);
            const partial = await cpu.run({ budget: 3n });
            equal(partial.reason, 'budget', 'short budget uses precise retirement');
            equal(partial.state.rip, head + 6n, 'budget stops at the XOR');
            equal(partial.state.registers[0], 8n, 'the shift has executed');

            cpu.setState(start);
            const exact = await cpu.run({ stop: end, budget: 5n });
            equal(exact.reason, 'budget', 'budget precedes a stop at the successor');
            equal(exact.state.rip, end, 'the exact budget retires the full prefix');

            cpu.setState(initial(head + 4n, { 0: 41n }, 2n));
            const interiorEntry = await cpu.run({ stop: end, budget: 6n });
            equal(interiorEntry.reason, 'stop', 'the interior remains a normal entry');
            equal(interiorEntry.retired, 3n, 'the interior entry executes its suffix');
            equal(interiorEntry.state.registers[0], 87n, 'the first two operations stay skipped');

            cpu.setState({ ...start, flags: 0x102n });
            const stepped = await cpu.run({ stop: end, budget: 6n });
            equal(stepped.reason, 'debug-exception', 'TF takes one instruction');
            equal(stepped.retired, 1n, 'TF observes only MOV');
            equal(stepped.state.rip, head + 2n, 'TF resumes at INC');

            cpu.writePhysicalMemory(0x9003n, Uint8Array.of(0xc8)); // DEC EAX
            cpu.setState(start);
            const changed = await cpu.run({ stop: end, budget: 6n });
            equal(changed.reason, 'stop', 'rewritten code reaches the same stop');
            equal(changed.state.registers[0], 1n, 'the old INC cannot execute');
          });
          same(guards, [1, 1, 2, 1], 'guard admits both live owners and refuses TF');
        } finally {
          restore();
        }
      },
    },
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
                  equal(calls.ready, 1, 'a successful checked read enables the cooldown');
                  check(calls.guarded > 0, 'later dword reads re-enter the guard in the same run');
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
    {
      name: 'paged 32-bit stack spills preserve dirty, segment, and partial budget boundaries',
      async run() {
        const head = 0x400000n;
        const data = 0x401000n;
        const spill = 0x402004n;
        const dest = 0x403000n;
        for (const layout of ['flat', 'read-only-pte', 'nonflat', 'expand-down'] as const) {
          const execution = context('compatibility');
          if (layout === 'nonflat') execution.segments.ss.base = 0x400000n;
          if (layout === 'expand-down') {
            execution.segments.ss.limit = 0x402000;
            execution.segments.ss.type = 'expand-down-read-write';
          }
          const leaf = layout === 'read-only-pte' ? 0xb025n : 0xb027n;
          await usingPaged(
            compiler,
            {
              code: PAGED_STACK_SPILL_LOOP,
              context: execution,
              bank: { ...BANK, cr0: BANK.cr0 | 0x10000n },
              words: [
                [0x4008n, 0xa027n],
                [0x4010n, leaf],
                [0x4018n, 0xc067n],
              ],
              spans: [
                { physical: 0xa000n, bytes: Uint8Array.of(0xff, 2, 3) },
                { physical: 0xb004n, bytes: little([0xffn, 0n], 4) },
                { physical: 0xc000n, bytes: new Uint8Array(8) },
              ],
            },
            async (cpu) => {
              const start = initial(
                head,
                {
                  0: 0n,
                  1: 3n,
                  2: 0n,
                  4: spill - 4n - execution.segments.ss.base,
                  6: dest,
                  7: data,
                },
                2n,
              );
              cpu.setState(start);
              const warm = await cpu.run({ budget: 1n });
              equal(warm.state.registers[0], 0xffn, layout);
              equal(word(cpu, 0x4010n), leaf, 'the preceding read leaves D clear');
              cpu.setState(start);
              const prefix = await cpu.run({ budget: 16n });
              equal(prefix.state.rip, head + 11n, 'continues at the original dword spill');
              if (layout === 'read-only-pte') {
                equal(prefix.reason, 'page-fault', layout);
                equal(prefix.retired, 4n, 'read and increment are the full committed prefix');
                equal(prefix.pageFault?.address, spill, 'the checked SS write faults at the spill');
                equal(prefix.state.registers[0], 0x100n, 'increment commits before the fault');
                equal(word(cpu, 0x4010n), leaf, 'the refused store leaves D clear');
                equal(word(cpu, 0xb004n), 0xffn, 'the refused store leaves the slot intact');
                equal(cpu.readPhysicalMemory(0xc000n, 1)[0], 0, 'later writes do not run');
                return;
              }
              equal(prefix.reason, 'budget', layout);
              equal(prefix.retired, 16n, 'one lap and exactly four more instructions');
              equal(prefix.state.registers[0], 0x101n, 'the next dword has been computed');
              equal(word(cpu, 0x4010n), 0xb067n, 'checked retry commits the guest dirty bit');
              equal(word(cpu, 0xb004n), 0x100n, 'only the first lap wrote the slot');
              same([...cpu.readPhysicalMemory(0xc000n, 3)], [0xff, 0, 0], 'one byte committed');
              const end = await cpu.run({ budget: 40n, stop: head + 27n });
              equal(end.reason, 'stop', layout);
              equal(end.retired, 20n, 'resume completes exactly the unpaid instructions');
              equal(end.state.flags, 0x46n, 'the final decrement supplies flags');
              equal(word(cpu, 0xb004n), 0x102n, 'the dword is little endian and not replayed');
              same([...cpu.readPhysicalMemory(0xc000n, 3)], [0xff, 3, 5], 'all bytes committed');
              if (layout === 'flat') {
                cpu.setState(start);
                const retained = await cpu.run({ budget: 40n, stop: head + 27n });
                equal(retained.reason, 'stop', 'dirty-ready retained spill');
                equal(retained.retired, 36n, 'three more full laps retire');
                equal(word(cpu, 0xb004n), 0x105n, 'the retained stores reach the stack backing');
                same([...cpu.readPhysicalMemory(0xc000n, 3)], [2, 6, 8], 'retained DS byte output');
              }
            },
          );
        }
      },
    },
    {
      name: 'paged 32-bit dword spill through an executable alias republishes the next opcode',
      async run() {
        const head = 0x400000n;
        await usingPaged(
          compiler,
          {
            code: PAGED_STACK_SPILL_LOOP,
            context: context('compatibility'),
            words: [
              [0x4008n, 0xa067n],
              [0x4010n, 0x9067n],
              [0x4018n, 0xc067n],
            ],
            spans: [
              { physical: 0xa000n, bytes: Uint8Array.of(0xff) },
              { physical: 0xc000n, bytes: new Uint8Array(8) },
            ],
          },
          async (cpu) => {
            // The spill replaces DEC ECX at code offset 24 with DEC EDX through SS.
            const start = initial(
              head,
              { 0: 0n, 1: 3n, 2: 0n, 4: 0x402014n, 6: 0x403000n, 7: 0x401000n },
              2n,
            );
            cpu.setState(start);
            equal((await cpu.run({ budget: 1n })).reason, 'budget', 'warm the alias translation');
            cpu.setState(start);
            const result = await cpu.run({ budget: 12n });
            equal(result.reason, 'budget', 'a complete lap reaches its successor');
            equal(result.retired, 12n, 'the source-changing instruction retires once');
            equal(result.state.rip, head, 'the replaced decrement feeds JNZ');
            equal(result.state.registers[1], 3n, 'the original DEC ECX does not execute');
            equal(result.state.registers[2], 0x82e57648n, 'the newly decoded DEC EDX executes');
            equal(result.state.flags, 0x86n, 'new decrement flags are visible');
            same(
              [...cpu.readPhysicalMemory(0x9018n, 4)],
              [0x4a, 0x75, 0xe5, 0x82],
              'modified code',
            );
          },
        );
      },
    },
    {
      name: 'paged byte store loops commit ordinary bytes and replay a refused write precisely',
      async run() {
        const head = 0x400000n;
        const input = 0x401000n;
        const output = 0x402000n;
        const data = new Uint8Array(0x1000);
        data.set([0x20, 0x44, 0x41, 0x17]);
        await usingPaged(
          compiler,
          {
            code: PAGED_BYTE_STORE_LOOP,
            bank: { ...BANK, cr0: BANK.cr0 | 0x10000n },
            words: [
              [0x4008n, 0xa067n],
              [0x4010n, 0xb027n],
              [0x4018n, 0xc025n],
            ],
            spans: [
              { physical: 0xa000n, bytes: data },
              { physical: 0xb000n, bytes: new Uint8Array(0x1000) },
              { physical: 0xc000n, bytes: Uint8Array.of(0x99) },
            ],
          },
          async (cpu) => {
            const start = (source: bigint, dest: bigint, count = 4n) =>
              initial(head, { 1: count, 6: dest, 7: source }, 2n);
            for (let run = 0; run < 2; run++) {
              cpu.setState(start(input, output));
              const result = await cpu.run({ budget: 40n, stop: head + 21n });
              equal(result.reason, 'stop', 'both conditional paths reach the successor');
              equal(result.retired, 34n, 'the two selected increments retire');
              equal(result.state.flags, 0x47n, 'the final comparison carry survives DEC');
              same(
                [...cpu.readPhysicalMemory(0xb000n, 4)],
                [0x20, 0x45, 0x42, 0x17],
                'one output byte per source byte',
              );
              equal(word(cpu, 0x4010n), 0xb067n, 'the first checked write set the dirty bit');
            }

            const untouched = output + 16n;
            cpu.setState(start(input, untouched));
            const stopped = await cpu.run({ budget: 40n, stop: head + 9n });
            equal(stopped.reason, 'stop', 'the taken edge stops at the canonical store label');
            equal(stopped.retired, 3n, 'the selected branch skips the increment');
            equal(cpu.readPhysicalMemory(0xb010n, 1)[0], 0, 'the stopped store is unstarted');
            const resumed = await cpu.run({ budget: 1n });
            equal(resumed.reason, 'budget', 'direct reentry retires exactly the store');
            equal(resumed.retired, 1n, 'the prior branch does not replay');
            equal(resumed.state.rip, head + 11n, 'the store commits its true successor');
            equal(cpu.readPhysicalMemory(0xb010n, 1)[0], 0x20, 'the reentered store commits once');

            cpu.setState(start(input, untouched));
            const partial = await cpu.run({ budget: 10n });
            equal(partial.reason, 'budget', 'the next lap runs only its funded prefix');
            equal(partial.retired, 10n, 'the clone and checked prefix share the budget');
            equal(partial.state.rip, head + 5n, 'partial replay reaches the second branch');
            equal(partial.state.registers[0], 0x44n, 'the next source byte was loaded');
            equal(partial.state.registers[1], 3n, 'the second latch is unstarted');

            cpu.setState({ ...start(input, untouched), flags: 0x102n });
            const stepped = await cpu.run({ budget: 40n });
            equal(stepped.reason, 'debug-exception', 'TF takes the checked entry at a warm head');
            equal(stepped.retired, 1n, 'single-step sees the first source load only');
            equal(stepped.state.rip, head + 3n, 'single-step resumes at the comparison');

            // Retain the read-only translation before trying the guarded store.
            cpu.setState(start(0x403000n, output + 8n, 1n));
            equal((await cpu.run({ budget: 12n, stop: head + 21n })).reason, 'stop', 'warm read');
            cpu.setState(start(input, output + 0xffen));
            const fault = await cpu.run({ budget: 40n });
            equal(fault.reason, 'page-fault', 'the third write retains guest write protection');
            equal(fault.retired, 21n, 'completed prefix excludes the refused store');
            equal(fault.state.rip, head + 9n, 'the fault is attributed to the original store');
            equal(
              fault.state.registers[0],
              0x42n,
              'the increment preceding the failed store is live',
            );
            equal(fault.state.registers[1], 2n, 'only the completed laps decrement ECX');
            equal(fault.state.flags, 6n, 'the original store sees the increment flags');
            equal(
              fault.pageFault?.address,
              0x403000n,
              'the checked store publishes its linear address',
            );
            same([...cpu.readPhysicalMemory(0xbffen, 2)], [0x20, 0x45], 'earlier stores survive');
            equal(cpu.readPhysicalMemory(0xc000n, 1)[0], 0x99, 'the denied byte stays intact');
            equal(word(cpu, 0x4018n), 0xc025n, 'the refused store does not set guest dirty state');
          },
        );
      },
    },
    {
      name: 'paged byte store loop re-enters after a fresh read and preserves a later write fault',
      async run() {
        const head = 0x400000n;
        const src = 0x401fffn;
        const denied = 0x404000n;
        const first = new Uint8Array(0x1000);
        first[0xfff] = 0x20;
        const second = new Uint8Array(32).fill(0x41);
        let replayReady = false;
        let guardedWrites = 0;
        const guardRefusals: bigint[] = [];
        const restore = observeArtifactImports((env) => {
          const ready = env.paged_loop_read_ready;
          const write = env.paged_loop_write_byte;
          if (typeof ready === 'function' && typeof write === 'function') {
            env.paged_loop_read_ready = (address: bigint, width: number) => {
              const result = ready(address, width);
              if (result === 1) replayReady = true;
              return result;
            };
            env.paged_loop_write_byte = (address: bigint, value: number) => {
              const result = write(address, value);
              if (replayReady && result === 1) guardedWrites++;
              if (replayReady && result === 0) guardRefusals.push(address);
              return result;
            };
          }
        });
        try {
          await usingPaged(
            compiler,
            {
              code: PAGED_BYTE_STORE_LOOP,
              bank: { ...BANK, cr0: BANK.cr0 | 0x10000n },
              words: [
                [0x4008n, 0xa027n],
                [0x4010n, 0xb007n],
                [0x4018n, 0xc067n],
                [0x4020n, 0xd025n],
              ],
              spans: [
                { physical: 0xa000n, bytes: first },
                { physical: 0xb000n, bytes: second },
                { physical: 0xc000n, bytes: new Uint8Array(0x1000) },
                { physical: 0xd000n, bytes: Uint8Array.of(0xa5) },
              ],
            },
            async (cpu) => {
              cpu.setState(initial(head, { 1: 20n, 6: denied - 16n, 7: src }, 2n));
              const fault = await cpu.run({ budget: 200n });
              check(replayReady, 'the cold source page arms re-entry after its checked read');
              check(guardedWrites > 0, 'the reentered guard writes to the retained destination');
              same(guardRefusals, [denied], 'the later write exits the guarded path once');
              equal(fault.reason, 'page-fault', 'the checked write reports protection');
              equal(fault.retired, 147n, 'only the completed laps and faulting prefix retire');
              equal(fault.state.rip, head + 9n, 'the refused store remains unretired');
              equal(fault.state.registers[0], 0x42n, 'the faulting prefix computes its value');
              equal(fault.state.registers[1], 4n, 'only completed laps decrement the count');
              equal(fault.state.flags, 6n, 'replay preserves the preceding increment flags');
              equal(fault.pageFault?.address, denied, 'the checked write publishes its address');
              same(
                [...cpu.readPhysicalMemory(0xcff0n, 16)],
                [0x20, ...Array<number>(15).fill(0x42)],
                'all successful bytes commit before the fault',
              );
              equal(cpu.readPhysicalMemory(0xd000n, 1)[0], 0xa5, 'the denied byte is untouched');
              equal(word(cpu, 0x4010n), 0xb027n, 'the source read sets only the accessed bit');
              equal(word(cpu, 0x4020n), 0xd025n, 'the denied write leaves the PTE intact');
            },
          );
        } finally {
          restore();
        }
      },
    },
    {
      name: 'paged Long64 scalar writes rearm checked reads and writes before a partial lap',
      async run() {
        const head = 0x400000n;
        const output = 0x403fd0n;
        // MOVZX EAX,[RDI]; ADD R8D,[RSI]; LEA RDI,[RDI+1]; DEC ECX; JZ done;
        // TEST EAX,EAX; JZ head; MOV [RDX],R8D; LEA RDX,[RDX+4]; JMP head; done: NOP.
        const code = '0fb607440306488d7f01ffc9740d85c074ee448902488d5204ebe590';
        const input = new Uint8Array(40).fill(1);
        input[0] = 0;
        const routes: string[] = [];
        const restore = observeArtifactImports((env) => {
          for (const name of [
            'paged_loop_read_ready',
            'paged_loop_read_dword',
            'paged_loop_write_ready',
            'paged_loop_write_dword',
          ]) {
            const helper = env[name];
            if (typeof helper !== 'function') continue;
            env[name] = (...args: (bigint | number)[]) => {
              const result = helper(...args);
              if (name === 'paged_loop_read_dword' ? result !== -1n : result === 1)
                routes.push(name);
              return result;
            };
          }
        });
        try {
          await usingPaged(
            compiler,
            {
              code,
              words: [
                [0x4008n, 0xa007n],
                [0x4010n, 0xb007n],
                [0x4018n, 0xc007n],
                [0x4020n, 0xd007n],
              ],
              spans: [
                { physical: 0xa000n, bytes: input },
                { physical: 0xb000n, bytes: little([3n], 4) },
                { physical: 0xc000n, bytes: new Uint8Array(8192) },
              ],
            },
            async (cpu) => {
              const start = initial(
                head,
                { 1: 40n, 2: output, 6: 0x402000n, 7: 0x401000n, 8: 0n },
                2n,
              );
              cpu.setState(start);
              equal(
                (await cpu.run({ budget: 1n })).reason,
                'budget',
                'warm only fetch and byte input',
              );

              cpu.setState(start);
              const result = await cpu.run({ budget: 254n });
              const readReady = routes.indexOf('paged_loop_read_ready');
              const firstWrite = routes.indexOf('paged_loop_write_dword');
              const writeReady = routes.lastIndexOf('paged_loop_write_ready');
              check(
                readReady >= 0 && routes.indexOf('paged_loop_read_dword') > readReady,
                'the cold coefficient retries through the checked ADD, then uses retained reads',
              );
              check(
                firstWrite >= 0 &&
                  firstWrite < writeReady &&
                  routes.lastIndexOf('paged_loop_write_dword') > writeReady,
                'guarded stores succeed on both sides of the checked page transition',
              );
              equal(result.reason, 'budget', 'the last lap stops before its store');
              equal(result.retired, 254n, 'one zero lap, 24 writing laps, and the final prefix');
              equal(result.state.rip, head + 0x12n, 'the last store remains unstarted');
              equal(result.state.registers[8], 78n, 'the final dword addition has retired');
              same(
                [...cpu.readPhysicalMemory(0xcfd0n, 100)],
                [...little([...Array.from({ length: 24 }, (_, i) => BigInt(6 + 3 * i)), 0n], 4)],
                'both backedges preserve the exact stores and untouched successor',
              );
            },
          );
        } finally {
          restore();
        }
      },
    },
    {
      name: 'paged Long64 adjacent dword additions preserve the first ADD on a second-page fault',
      async run() {
        const head = 0x400000n;
        const data = 0x401000n;
        // MOVZX EAX,[RDI]; ADD R8D,[RSI]; ADD R9D,[RSI+4]; LEA RDI,[RDI+1]; DEC ECX;
        // JZ done; TEST EAX,EAX; JZ head; MOV [RDX],R8D; LEA RDX,[RDX+4]; JMP head; done: NOP.
        const code = '0fb60744030644034e04488d7f01ffc9740d85c074ea448902488d5204ebe190';
        const bytes = new Uint8Array(4096);
        bytes.set(little([7n, 13n], 4), 0x100);
        bytes.set(little([7n], 4), 0xffc);
        const spans: boolean[] = [];
        const restore = observeArtifactImports((env) => {
          const span = env.paged_loop_read_span;
          if (typeof span === 'function') {
            env.paged_loop_read_span = (address: bigint, width: number, segment: number) => {
              const pointer = span(address, width, segment);
              if (width === 8) spans.push(pointer !== 0);
              return pointer;
            };
          }
        });
        try {
          for (const split of [false, true]) {
            await usingPaged(
              compiler,
              {
                code,
                words: [[0x4008n, 0xa007n]],
                spans: [{ physical: 0xa000n, bytes }],
              },
              async (cpu) => {
                const address = data + (split ? 0xffcn : 0x100n);
                const start = initial(
                  head,
                  { 1: 1n, 2: data, 6: address, 7: data, 8: 0xffff_ffffn, 9: 0xffff_fffen },
                  2n,
                );
                cpu.setState(start);
                const prime = await cpu.run({ budget: 2n });
                equal(prime.retired, 2n, 'warm the first load through the checked path');
                cpu.setState(start);
                const result = await cpu.run({ stop: head + 0x1fn, budget: 21n });
                equal(result.state.registers[8], 6n, 'the first ADD commits in both cases');
                if (split) {
                  equal(result.reason, 'page-fault', 'the unmapped next page faults');
                  equal(result.state.rip, head + 6n, 'RIP points to the faulting ADD');
                  equal(result.retired, 2n, 'only the prefix retires');
                  equal(
                    result.state.registers[9],
                    0xffff_fffen,
                    'the second destination is intact',
                  );
                  equal(result.state.flags & 0x841n, 1n, 'the first ADD leaves carry set');
                  equal(cpu.readPagingState().cr2, address + 4n, 'CR2 names the second operand');
                } else {
                  equal(result.reason, 'stop', 'the same-leaf pair completes');
                  equal(result.retired, 6n, 'both ADDs retire independently');
                  equal(result.state.registers[9], 11n, 'the second ADD reads the next dword');
                }
              },
            );
          }
          same(spans, [true, false], 'the guard admits one leaf and refuses the crossing pair');
        } finally {
          restore();
        }
      },
    },
    {
      name: 'paged packed SSE re-enters after a checked split and preserves a later split fault',
      async run() {
        const head = 0x400000n;
        const source = 0x401ff8n;
        const denied = 0x403000n;
        const output = 0x404000n;
        // MOVUPS; CMPNLEPS; MOVMSKPS; TEST; LEA ESI,[ESI+16]; JNZ head; store; load; JMP head.
        const code = '0f100e0fc2c8060f50c185c08d761075ef890f8b13ebe9';
        const input = new Uint8Array(0x2000);
        input.fill(0xff, 0x1ff8);
        let splitReady = false;
        let retainedAfterSplit = 0;
        const refused: bigint[] = [];
        const restore = observeArtifactImports((env) => {
          const ready = env.paged_loop_split_read_ready;
          const read = env.paged_loop_block_host;
          const window = env.paged_loop_block_window;
          if (
            typeof ready === 'function' &&
            typeof read === 'function' &&
            typeof window === 'function'
          ) {
            env.paged_loop_split_read_ready = (address: bigint, width: number) => {
              const result = ready(address, width);
              if (address === source && width === 16 && result === 1) splitReady = true;
              return result;
            };
            env.paged_loop_block_window = (address: bigint) => {
              const result = window(address) as bigint;
              if (splitReady && result !== 0n) retainedAfterSplit++;
              return result;
            };
            env.paged_loop_block_host = (address: bigint) => {
              const result = read(address);
              if (splitReady && result === 0) refused.push(address);
              return result;
            };
          }
        });
        const replay = (mode: 'long64' | 'compatibility') =>
          usingPaged(
            compiler,
            {
              code,
              ...(mode === 'compatibility' ? { context: context(mode) } : {}),
              words: [
                [0x4008n, 0xa007n],
                [0x4010n, 0xb007n],
                [0x4020n, 0xc007n],
              ],
              spans: [
                { physical: 0xa000n, bytes: input },
                { physical: 0xc000n, bytes: little([0x1234_0000_0000n]) },
              ],
            },
            async (cpu) => {
              cpu.setState(
                initial(head, { 1: 0x12345678n, 3: output + 4n, 6: source, 7: output }, 2n),
              );
              const fault = await cpu.run({ budget: 2400n });
              check(
                splitReady && retainedAfterSplit > 100,
                mode + ' checked split permits windows, which must refill after the scalar arm',
              );
              same(refused, [denied - 8n], 'one later split returns to the checked operand');
              equal(fault.reason, 'page-fault', 'the second leaf is missing');
              equal(fault.retired, 2304n, '256 complete laps retire');
              equal(fault.state.rip, head, 'the original MOVUPS remains unretired');
              equal(fault.state.registers[6], denied - 8n, 'the faulting lap does not advance');
              equal(fault.state.flags, 0x46n, 'the previous TEST flags survive');
              equal(cpu.readXmmState()[1], 0n, 'the first half does not publish on a split fault');
              equal(cpu.readPagingState().cr2, denied, 'CR2 points to the denied half');
              equal(word(cpu, 0xc000n), 0x1234_1234_5678n, 'preceding stores and metadata survive');
              same(
                [word(cpu, 0x4008n), word(cpu, 0x4010n), word(cpu, 0x4018n), word(cpu, 0x4020n)],
                [0xa027n, 0xb027n, 0n, 0xc067n],
                'only successfully used leaves acquire accessed and dirty bits',
              );
            },
          );
        try {
          await replay('long64');
          splitReady = false;
          retainedAfterSplit = 0;
          refused.length = 0;
          await replay('compatibility');
        } finally {
          restore();
        }
      },
    },
    {
      name: 'paged register loop preserves checked reentry and observation boundaries',
      async run() {
        const head = 0x400000n;
        const middle = head + 3n;
        // ADD EAX,1; SUB ECX,1; JNE head.
        await usingPaged(compiler, { code: '83c00183e90175f8' }, async (cpu) => {
          const start = () => initial(head, { 0: 0n, 1: 3n }, 2n);
          cpu.setState(start());
          const partial = await cpu.run({ budget: 4n });
          equal(partial.reason, 'budget', 'partial second lap stops at its next instruction');
          equal(partial.retired, 4n, 'three instructions then the next addition');
          equal(partial.state.rip, middle, 'precise second-lap continuation');
          equal(partial.state.registers[0], 2n, 'both additions completed');
          equal(partial.state.registers[1], 2n, 'only the first subtraction completed');
          equal(partial.state.flags, 2n, 'a partial lap publishes the checked addition flags');
          const resumed = await cpu.run({ budget: 10n, stop: head + 8n });
          equal(resumed.reason, 'stop', 'resume directly at the saved middle entry');
          equal(resumed.retired, 5n, 'the prior addition does not replay');
          equal(resumed.state.registers[0], 3n, 'one addition per lap');

          // A write outside the source range clears the retained paging cache.
          cpu.writePhysicalMemory(0x9fffn, Uint8Array.of(0x82));
          cpu.setState(start());
          const complete = await cpu.run({ budget: 10n, stop: head + 8n });
          equal(complete.reason, 'stop', 'funded laps finish after a cold cached entry');
          equal(complete.resources.compilations, 0, 'the cached code owner survives the write');
          equal(complete.retired, 9n, 'three full laps retire');
          equal(complete.state.registers[0], 3n, 'all three additions complete');
          equal(complete.state.flags, 0x46n, 'loop exit retains the last arithmetic flags');

          cpu.setState(start());
          const stopped = await cpu.run({ budget: 10n, stop: middle });
          equal(stopped.reason, 'stop', 'stop wins inside a funded lap');
          equal(stopped.retired, 1n, 'only the addition completed');

          cpu.setState({ ...start(), flags: 0x102n });
          const stepped = await cpu.run({ budget: 10n });
          equal(stepped.reason, 'debug-exception', 'TF rejects a funded warm lap');
          equal(stepped.retired, 1n, 'single-step observes the first completion');
          equal(stepped.state.rip, middle, 'single-step reports the successor');

          cpu.setState({ ...start(), flags: 0x102n });
          const budgetStep = await cpu.run({ budget: 1n });
          equal(budgetStep.reason, 'debug-exception', 'single-step precedes exact budget return');

          cpu.setDebugState({ addresses: [head, 0n, 0n, 0n], dr6: 0n, dr7: 0x401n });
          cpu.setState({ ...start(), flags: 0x10002n });
          const broke = await cpu.run({ budget: 10n });
          equal(broke.reason, 'debug-exception', 'execute breakpoint wins on the taken backedge');
          equal(broke.retired, 3n, 'RF suppressed the initial breakpoint only');
          equal(broke.state.rip, head, 'backedge reached the original entry');
          equal(broke.state.flags & 0x10000n, 0n, 'RF was consumed before the backedge');

          cpu.setDebugState({ ...cpu.readDebugState(), dr7: 0n });
          cpu.setState({ ...start(), flags: 0x202n, interruptibility: { sti: true, ss: false } });
          cpu.requestInterrupt(0x21);
          const irq = await cpu.run({ budget: 10n, eventDelivery: { budget: 0n } });
          equal(irq.reason, 'event-budget', 'eligible IRQ is captured after STI shadow expires');
          equal(irq.retired, 1n, 'interrupt is observed after the first addition');
          equal(irq.state.rip, middle, 'IRQ returns to the correct loop entry');
        });
      },
    },
    {
      name: 'paged 32-bit register loop preserves flags and high banks with a nonzero code segment',
      async run() {
        const head = 0x3fffd0n;
        const exit = head + 8n;
        const execution = context('compatibility');
        execution.segments.cs.base = 0x30n;
        execution.segments.cs.limit = Number(exit - 1n);
        // SHL EAX,1; ADC EBX,0; DEC ECX; JNZ head.
        await usingPaged(
          compiler,
          { code: 'd1e083d3004975f8', context: execution },
          async (cpu) => {
            const start = initial(
              head,
              { 0: 0x1234567880000000n, 1: 0xabcdef0000000003n, 3: 0x9999999900000005n },
              2n,
            );
            cpu.setState(start);
            const result = await cpu.run({ budget: 13n, stop: exit });
            equal(result.reason, 'stop', 'stop after the last valid code-segment byte');
            equal(result.retired, 12n, 'three four-instruction laps retire');
            equal(result.state.rip, exit, 'the saved PC is still an offset from CS');
            equal(result.state.registers[0], 0x1234567800000000n, 'SHL keeps the upper bank');
            equal(result.state.registers[1], 0xabcdef0000000000n, 'DEC keeps the upper bank');
            equal(result.state.registers[3], 0x9999999900000006n, 'ADC observes the shift carry');
            equal(result.state.flags, 0x46n, 'DEC publishes the exit flags');
          },
        );
      },
    },
    {
      name: 'paged dword loop keeps the completed prefix when its fourth read faults',
      async run() {
        const head = 0x400000n;
        const badRead = head + 13n;
        const badData = 0x402000n;
        // Warm [RDI] before the lap. Three loads and an ADD precede the failing MOV EBX,[RSI].
        const code = '8b078b078b4f048b570883c0018b1e83ed0175ee';
        for (const reason of ['page-fault', 'debug-exception', 'event-budget'] as const) {
          await usingPaged(
            compiler,
            {
              code,
              words: [
                [0x1000n, 0x2027n],
                [0x2000n, 0x3027n],
                [0x3010n, 0x4027n],
                [0x4000n, 0x9027n],
                [0x4008n, 0xa027n],
              ],
              spans: [{ physical: 0xa000n, bytes: little([0x12345678ffffffffn, 42n]) }],
            },
            async (cpu) => {
              const start = initial(head, { 3: 0x1234n, 5: 2n, 6: badData, 7: 0x401000n }, 2n);
              if (reason === 'debug-exception')
                cpu.setDebugState({ addresses: [badRead, 0n, 0n, 0n], dr6: 0n, dr7: 0x401n });
              if (reason === 'event-budget') {
                cpu.setState({
                  ...start,
                  flags: 0x202n,
                  interruptibility: { sti: true, ss: false },
                });
                cpu.requestInterrupt(0x21);
                const irq = await cpu.run({ budget: 20n, eventDelivery: { budget: 0n } });
                equal(irq.reason, reason, 'IRQ leaves before the first lap');
                equal(irq.retired, 1n, 'the STI shadow is one instruction');
                equal(irq.state.rip, head + 2n, 'restart at the first lap instruction');
                return;
              }
              cpu.setState(start);
              const result = await cpu.run({ budget: 20n });
              equal(result.reason, reason, 'precise checked reason at the fourth read');
              equal(result.retired, 5n, 'only the preload and completed prefix retire');
              equal(result.state.rip, badRead, 'restart at the faulting read');
              equal(result.state.registers[0], 0n, 'the wrapping addition completed');
              equal(result.state.registers[1], 0x12345678n, 'second read completed');
              equal(result.state.registers[2], 42n, 'third read completed');
              equal(result.state.registers[3], 0x1234n, 'the fourth destination remains unchanged');
              equal(result.state.registers[5], 2n, 'the latch has not run');
              equal(
                result.state.flags & 0x8d7n,
                0x57n,
                'fault keeps the preceding ADD status flags',
              );
              if (reason === 'page-fault')
                equal(result.pageFault?.address, badData, 'the operand supplies the PF address');
              else
                equal(cpu.readPagingState().cr2, 0n, 'execute #DB precedes the operand page walk');
            },
          );
        }
      },
    },
    {
      name: 'paged rotated memory compare keeps branch, stop and refusal boundaries exact',
      async run() {
        const prefix = 0x400000n;
        const head = prefix + 8n;
        const compare = prefix + 19n;
        const exit = prefix + 36n;
        // The upper-half path branches backward to the prefix, which can fall
        // through to the head. The lower-half path branches directly to the head.
        const code = '4489f74139fb73c889fb4429dbd1eb468d341b423934b173e74101db41ffc34139fb72e4';
        const data = new Uint8Array(4096);
        data.set(little([0n, 1n, 2n, 3n], 4));
        await usingPaged(
          compiler,
          {
            code,
            words: [[0x4008n, 0xa027n]],
            spans: [{ physical: 0xa000n, bytes: data }],
          },
          async (cpu) => {
            const start = (base: bigint, needle: bigint, upper = 4n) =>
              initial(head, { 1: base, 6: needle, 7: upper, 11: 0n }, 2n);
            cpu.setState(start(0x401000n, 2n));
            equal((await cpu.run({ budget: 5n })).state.rip, compare + 4n, 'warm code and data');
            cpu.setState(start(0x401000n, 2n));
            const done = await cpu.run({ budget: 32n, stop: exit });
            equal(done.reason, 'stop', 'both halves reach the common falling exit');
            equal(done.retired, 19n, 'nine upper-half steps and ten lower-half steps');
            equal(done.state.rip, exit, 'falling exit is the original next byte');
            equal(done.state.registers[7], 2n, 'rotated prefix lowered the upper bound');
            equal(done.state.registers[11], 2n, 'lower-half path raised the lower bound');
            equal(done.state.flags & 0x8d7n, 0x46n, 'the final bound compare sets ZF');

            const takenExit = prefix - 48n;
            cpu.setState(start(0x401000n, 0n, 1n));
            const taken = await cpu.run({ budget: 32n, stop: takenExit });
            equal(taken.reason, 'stop', 'the rotated prefix can take the other exit');
            equal(taken.retired, 9n, 'the exiting prefix retires its compare and branch');
            equal(taken.state.rip, takenExit, 'stop precedes fetching the unmapped exit');
            equal(taken.state.registers[7], 0n, 'the exiting prefix commits the empty interval');
            equal(taken.state.flags & 0x8d7n, 0x46n, 'the exiting compare publishes ZF');

            cpu.setState(start(0x401000n, 2n));
            const partial = await cpu.run({ budget: 10n });
            equal(partial.reason, 'budget', 'a shorter first lap leaves one checked instruction');
            equal(partial.retired, 10n, 'the complete upper path and next MOV retire');
            equal(partial.state.rip, head + 2n, 'the checked entry stops before the next SUB');
            equal(partial.state.flags & 0x8d7n, 0x93n, 'the first latch flags survive checked MOV');

            cpu.setState(start(0x401000n, 2n));
            const stopped = await cpu.run({ budget: 32n, stop: prefix });
            equal(stopped.reason, 'stop', 'stop applies to the rotated prefix');
            equal(stopped.retired, 6n, 'wait at the branch target without entering the prefix');
            equal(stopped.state.rip, prefix, 'stop points to the unstarted prefix');
            equal(stopped.state.registers[7], 4n, 'the prefix did not change the upper bound');

            cpu.setState(start(0x401ff4n, 1n));
            const denied = await cpu.run({ budget: 32n });
            equal(denied.reason, 'page-fault', 'second memory compare faults exactly');
            equal(denied.retired, 14n, 'first lap and the second arithmetic prefix retire once');
            equal(denied.state.rip, compare, 'retry starts at the faulting memory compare');
            equal(denied.state.registers[11], 3n, 'first lap bound is preserved');
            equal(denied.state.registers[14], 3n, 'second prefix formed the faulting index');
            equal(denied.state.flags & 0x8d7n, 0x47n, 'incoming SHR flags survive the refused CMP');
            same(
              denied.pageFault,
              {
                vector: 14,
                instructionPc: compare,
                address: 0x402000n,
                errorCode: 0,
                access: 'read',
              },
              'checked operand supplies the exact fault packet',
            );
          },
        );
      },
    },
    {
      name: 'paged dword graph crosses retained leaves and physical slices before exact refusal',
      async run() {
        const head = 0x400000n;
        const first = 0x401ff4n;
        const second = 0x402000n;
        // CMP EAX,[EDI]; JBE skip; ADD EBX,1; ADD EDI,4; DEC ECX; JNZ head.
        const code = '3b07760383c30183c704ffc975f2';
        for (const layout of ['local', 'global', 'split-backing'] as const) {
          const previous = new Uint8Array(15);
          previous.set(little([1n, 3n, 5n], 4), 3);
          const next = little([0xffffffffn, 1n], 4);
          await usingPaged(
            compiler,
            {
              code,
              context: context('compatibility'),
              bank: { ...BANK, cr4: BANK.cr4 | 0x80n },
              words: [
                [0x4008n, 0xa027n],
                [0x4010n, layout === 'global' ? 0xb127n : 0xb027n],
              ],
              spans: [
                { physical: 0xaff1n, bytes: previous },
                ...(layout === 'split-backing'
                  ? [
                      { physical: 0xb000n, bytes: next.slice(0, 2) },
                      { physical: 0xb002n, bytes: next.slice(2) },
                    ]
                  : [{ physical: 0xb000n, bytes: next }]),
              ],
            },
            async (cpu) => {
              const start = (source: bigint) =>
                initial(head, { 0: 4n, 1: 6n, 3: 0n, 7: source }, 2n);
              for (const address of [second, first]) {
                cpu.setState(start(address));
                const warm = await cpu.run({ budget: 1n });
                equal(warm.reason, 'budget', 'both data leaves are retained by the checked reads');
              }
              cpu.setState(start(first));
              const partial = await cpu.run({ budget: 7n });
              equal(partial.reason, 'budget', layout);
              equal(partial.retired, 7n, 'one full lap and the next checked CMP retire');
              equal(partial.state.rip, head + 2n, 'the partial lap resumes at its JBE');
              cpu.setState(start(first));
              const result = await cpu.run({ budget: 40n });
              equal(result.reason, 'physical-memory-fault', layout);
              equal(result.retired, 28n, 'failure excludes the sixth compare');
              equal(result.state.rip, head, 'the exact checked entry retries the compare');
              equal(result.state.registers[1], 1n, 'five laps retired before the refusal');
              equal(result.state.registers[3], 3n, 'all three unsigned matches committed');
              equal(result.state.registers[7], second + 8n, 'the faulting offset stays live');
              equal(result.state.flags, 2n, 'incoming decrement flags survive the failing compare');
              equal(
                result.physicalMemoryFault?.phase,
                'final-access',
                'the fault follows the walk',
              );
            },
          );
        }
      },
    },
    {
      name: 'paged compatibility rotated memory compare preserves exact branch and fault state',
      async run() {
        const prefix = 0x3fffd0n;
        const head = prefix + 6n;
        const compare = prefix + 15n;
        const exit = prefix + 27n;
        const execution = context('compatibility');
        execution.segments.cs.base = 0x30n;
        const code = '89d339dd73ba89de29eed1ee8d142e393c9173ec01f54539dd72eb';
        const table = new Uint8Array(4096);
        table.set(little([0n, 1n, 2n, 3n], 4));
        await usingPaged(
          compiler,
          {
            code,
            context: execution,
            words: [[0x4008n, 0xa027n]],
            spans: [{ physical: 0xa000n, bytes: table }],
          },
          async (cpu) => {
            const start = (base: bigint, needle: bigint, upper = 4n) =>
              initial(head, { 1: base, 2: 0n, 3: upper, 5: 0n, 6: 0n, 7: needle }, 2n);
            cpu.setState(start(0x401000n, 2n));
            equal(
              (await cpu.run({ budget: 5n })).state.rip,
              compare + 3n,
              'warm the first compare',
            );
            cpu.setState(start(0x401000n, 2n));
            const done = await cpu.run({ budget: 32n, stop: exit });
            equal(done.reason, 'stop', 'both halves reach the falling exit');
            equal(done.retired, 19n, 'nine upper and ten lower steps');
            equal(done.state.rip, exit, 'the falling PC remains CS relative');
            equal(done.state.registers[3], 2n, 'upper bound');
            equal(done.state.registers[5], 2n, 'lower bound');
            equal(done.state.flags & 0x8d7n, 0x46n, 'bound comparison flags');

            const outside = prefix - 64n;
            cpu.setState(start(0x401000n, 0n, 1n));
            const taken = await cpu.run({ budget: 32n, stop: outside });
            equal(taken.reason, 'stop', 'the rotated prefix can take the outside edge');
            equal(taken.retired, 9n, 'the exiting prefix retires');
            equal(taken.state.rip, outside, 'stop wins before the next fetch');
            equal(taken.state.flags & 0x8d7n, 0x46n, 'the exiting compare publishes ZF');

            cpu.setState(start(0x401000n, 2n));
            const partial = await cpu.run({ budget: 10n });
            equal(partial.reason, 'budget', 'a short upper path leaves one checked instruction');
            equal(partial.retired, 10n, 'the complete upper path and next MOV retire');
            equal(partial.state.rip, head + 2n, 'resume before the second SUB');
            equal(partial.state.flags & 0x8d7n, 0x93n, 'the first latch flags survive checked MOV');

            cpu.setState(start(0x401000n, 2n));
            const stopped = await cpu.run({ budget: 32n, stop: prefix });
            equal(stopped.reason, 'stop', 'the rotated prefix is a stop boundary');
            equal(stopped.retired, 6n, 'prefix effects have not occurred');
            equal(stopped.state.registers[3], 4n, 'the upper bound remains unchanged');

            cpu.setState(start(0x401ff4n, 1n));
            const denied = await cpu.run({ budget: 32n });
            equal(denied.reason, 'page-fault', 'the second compare faults');
            equal(denied.retired, 14n, 'first lap and second arithmetic prefix retire once');
            equal(denied.state.rip, compare, 'retry starts at the unretired compare');
            equal(denied.state.registers[5], 3n, 'first lap lower bound is visible');
            equal(denied.state.registers[2], 3n, 'second prefix formed the faulting index');
            equal(denied.state.flags & 0x8d7n, 0x47n, 'incoming SHR flags survive');
            equal(
              denied.pageFault?.address,
              0x402000n,
              'checked replay publishes the exact address',
            );

            cpu.setState(start(0xfffffff6n, 1n));
            const wrap = await cpu.run({ budget: 32n });
            equal(wrap.reason, 'segment-fault', 'a dword crossing 4GiB faults before walking');
            equal(wrap.retired, 4n, 'only the arithmetic prefix retires');
            equal(wrap.state.rip, compare, 'wrapped operand keeps the original entry');
            equal(wrap.segmentFault?.segment, 'ds', 'segment admission gives the fault');
            equal(
              wrap.segmentFault?.offset,
              0xfffffffen,
              'the exact wrapped dword starting offset',
            );
          },
        );
      },
    },
    {
      name: 'paged MOVZX keeps the exact byte loop prefix before a missed read',
      async run() {
        const head = 0x400000n;
        const badLinear = head + 0x2000n;
        for (const [fs, ac] of [
          [false, false],
          [false, true],
          [true, true],
        ] as const) {
          const execution = snapshot();
          execution.cpl = 3;
          if (fs) execution.segments.fs.base = head;
          const base = fs ? 0n : head;
          const flags = ac ? 0x40002n : 2n;
          await usingPaged(
            compiler,
            {
              // Warm three bytes in [RDI+1]. The fourth lap read uses absent [RSI].
              code: fs
                ? '640fb607640fb607640fb64f01640fb6570283c001640fb61e83ed0175e6'
                : '0fb6070fb6070fb64f010fb6570283c0010fb61e83ed0175ea',
              context: execution,
              bank: { ...BANK, cr0: BANK.cr0 | 0x40000n },
              words: [
                [0x1000n, 0x2027n],
                [0x2000n, 0x3027n],
                [0x3010n, 0x4027n],
                [0x4000n, 0x9027n],
                [0x4008n, 0xa027n],
              ],
              spans: [{ physical: 0xa000n, bytes: Uint8Array.of(0, 0xff, 0x34, 42) }],
            },
            async (cpu) => {
              cpu.setState(
                initial(
                  head,
                  {
                    0: 0xffffffffffffffffn,
                    3: 0x1234n,
                    5: 2n,
                    6: base + 0x2000n,
                    7: base + 0x1001n,
                  },
                  flags,
                ),
              );
              const result = await cpu.run({ budget: 20n });
              equal(result.reason, 'page-fault', 'the fourth read misses with AC clear or set');
              equal(
                result.pageFault?.address,
                badLinear,
                'non-identity FS resolves the same operand',
              );
              equal(result.pageFault?.errorCode, 4, 'not-present user read');
              equal(result.retired, 5n, 'preload and four completed lap instructions');
              equal(result.state.rip, head + (fs ? 21n : 17n), 'failed MOVZX remains unretired');
              equal(result.state.registers[0], 256n, 'read 0xff, zero-extend, then add one');
              equal(result.state.registers[1], 0x34n, 'second byte committed');
              equal(result.state.registers[2], 42n, 'third byte committed');
              equal(result.state.registers[3], 0x1234n, 'miss publishes no destination');
              equal(
                result.state.flags & 0x408d7n,
                (flags & 0x40000n) | 0x16n,
                'ADD and AC survive',
              );
            },
          );
        }
      },
    },
    {
      name: 'paged MOVZX inner loop preserves state across repeat visits and a retained miss',
      async run() {
        const head = 0x400000n;
        // Preload, then two outer visits of two inner laps with four MOVZX loads.
        const code = '0fb607b9020000000fb6070fb657010fb65f020fb677034101c0ffc975eaffcd75e1';
        for (const miss of [false, true]) {
          const data = miss ? 0x401ffdn : 0x401000n;
          await usingPaged(
            compiler,
            {
              code,
              words: [
                [0x1000n, 0x2027n],
                [0x2000n, 0x3027n],
                [0x3010n, 0x4027n],
                [0x4000n, 0x9027n],
                [0x4008n, 0xa027n],
                [0x4010n, 0xb027n],
              ],
              spans: miss
                ? [
                    { physical: 0xaffdn, bytes: Uint8Array.of(0xff, 10, 11) },
                    { physical: 0xb000n, bytes: Uint8Array.of(12) },
                  ]
                : [{ physical: 0xa000n, bytes: Uint8Array.of(0xff, 10, 11, 12) }],
            },
            async (cpu) => {
              cpu.setState(initial(head, { 5: 2n, 7: data, 8: 0n }, 2n));
              const result = await cpu.run({ budget: 36n, stop: head + 34n });
              equal(result.reason, 'stop', 'both outer visits finish');
              equal(result.resources.nativeEntries, 1, 'both visits use one generated activation');
              equal(result.retired, 35n, 'all inner laps retire exactly once');
              equal(result.state.registers[1], 0n, 'the inner counter finishes');
              equal(result.state.registers[5], 0n, 'the outer counter finishes');
              equal(result.state.registers[6], 12n, 'the fourth read commits on both mapped pages');
              equal(
                result.state.registers[8],
                1020n,
                'the four guarded or checked additions commit',
              );
              equal(result.state.flags, 0x46n, 'the outer loop exit publishes the last DEC');
            },
          );
        }
      },
    },
    {
      name: 'paged affine byte loop agrees across direct and checked span boundaries',
      async run() {
        const head = 0x400000n;
        // Preload, then four indexed MOVZX loads and a four-byte induction step.
        const code = '0fb60731c90fb6040f0fb6540f010fb65c0f020fb6740f034101c04883c1044883f90875e0';
        const bytes = Uint8Array.of(0xff, 1, 2, 3, 4, 5, 6, 7);
        for (const layout of [
          'local',
          'split-ram',
          'split-tlb',
          'contiguous-tlb',
          'global',
          'denied',
        ] as const) {
          const splitTlb = layout === 'split-tlb';
          const contiguousTlb = layout === 'contiguous-tlb';
          const crossesTlb = splitTlb || contiguousTlb;
          const denied = layout === 'denied';
          const global = layout === 'global';
          const splitRam = denied || layout === 'split-ram';
          await usingPaged(
            compiler,
            {
              code,
              bank: { ...BANK, cr4: global ? BANK.cr4 | 0x80n : BANK.cr4 },
              words: [
                [0x1000n, 0x2027n],
                [0x2000n, 0x3027n],
                [0x3010n, 0x4027n],
                [0x4000n, 0x9027n],
                [0x4008n, global ? 0xa127n : 0xa027n],
                [0x4010n, 0xb027n],
              ],
              spans:
                splitRam || splitTlb
                  ? [
                      { physical: splitTlb ? 0xaffcn : 0xa000n, bytes: bytes.slice(0, 4) },
                      {
                        physical: splitTlb ? 0xb000n : 0xa004n,
                        bytes: bytes.slice(4),
                        read: !denied,
                      },
                    ]
                  : [{ physical: contiguousTlb ? 0xaffcn : 0xa000n, bytes }],
            },
            async (cpu) => {
              const start = initial(head, { 7: crossesTlb ? 0x401ffcn : 0x401000n, 8: 0n }, 2n);
              if (contiguousTlb) {
                cpu.setState(initial(head, { 7: 0x402000n }, 2n));
                equal((await cpu.run({ budget: 1n })).reason, 'budget', 'retain the second leaf');
              }
              cpu.setState(start);
              const result = await cpu.run({ budget: 20n, stop: head + 37n });
              equal(result.reason, denied ? 'physical-memory-fault' : 'stop', layout);
              equal(result.retired, denied ? 10n : 18n, `${layout} retirement`);
              equal(result.state.rip, head + (denied ? 5n : 37n), `${layout} next PC`);
              equal(result.state.registers[0], denied ? 0xffn : 4n, 'failed byte does not publish');
              equal(result.state.registers[1], denied ? 4n : 8n, 'induction counter');
              equal(result.state.registers[6], denied ? 3n : 7n, 'last successful byte');
              equal(result.state.registers[8], denied ? 255n : 259n, 'exact sum with byte 0xff');
              equal(result.state.flags, denied ? 0x97n : 0x46n, 'last comparison decides flags');
              if (layout !== 'local' && !contiguousTlb) return;
              cpu.setState(start);
              const partial = await cpu.run({ budget: 11n });
              equal(partial.reason, 'budget', 'partial second lap takes the checked entry');
              equal(partial.retired, 11n, 'one partial-lap byte retires');
              equal(partial.state.rip, head + 9n, 'resume after the checked byte');
              equal(partial.state.registers[0], 4n, 'partial-lap byte is visible');
              equal(partial.state.flags, 0x97n, 'partial lap preserves the prior comparison');
            },
          );
        }
      },
    },
    {
      name: 'paged compatibility reads preserve flat data and checked segment boundaries',
      async run() {
        const head = 0x3fffd0n;
        for (const layout of ['dword', 'byte', 'nonflat', 'limit', 'wrap', 'no-read'] as const) {
          const execution = context('compatibility');
          execution.segments.cs.base = 0x30n;
          if (layout === 'nonflat') execution.segments.ds.base = 0x400000n;
          if (layout === 'limit') execution.segments.ds.limit = 0x40100b;
          const byte = layout === 'byte';
          const payload = byte
            ? Uint8Array.of(0xff, 0x34, 42, 7)
            : little([0x12345678ffffffffn, 0x000000070000002an]);
          const code = byte
            ? '0fb6070fb6070fb64f010fb6570283c0010fb61e83ed0175ea'
            : '8b078b078b4f048b570883c0018b1e83ed0175ee';
          const data = layout === 'nonflat' ? 0x1000n : 0x401000n;
          const fourth = layout === 'wrap' ? 0xfffffffen : data + (byte ? 3n : 12n);
          const fault =
            layout === 'limit' || layout === 'wrap'
              ? 'segment-fault'
              : layout === 'no-read'
                ? 'physical-memory-fault'
                : undefined;
          await usingPaged(
            compiler,
            {
              code,
              context: execution,
              words: [
                [0x1000n, 0x2027n],
                [0x2000n, 0x3027n],
                [0x3010n, 0x4027n],
                [0x4000n, 0x9027n],
                [0x4008n, 0xa027n],
              ],
              spans:
                layout === 'no-read'
                  ? [
                      { physical: 0xa000n, bytes: payload.slice(0, 12) },
                      { physical: 0xa00cn, bytes: payload.slice(12), read: false },
                    ]
                  : [{ physical: 0xa000n, bytes: payload }],
            },
            async (cpu) => {
              const high = 0xaabbccdd00000000n;
              cpu.setState(
                initial(
                  head,
                  {
                    0: high,
                    2: 0n,
                    3: high | 0x1234n,
                    5: 2n,
                    6: high | fourth,
                    7: high | data,
                  },
                  2n,
                ),
              );
              const result = await cpu.run({ budget: 16n, stop: head + BigInt(code.length / 2) });
              equal(result.reason, fault ?? 'stop', layout);
              equal(result.retired, fault ? 5n : 15n, 'preserve exact iteration or prefix');
              equal(
                result.state.rip,
                head + (fault ? 13n : BigInt(code.length / 2)),
                'CS-relative PC',
              );
              equal(
                result.state.registers[0],
                high | (byte ? 256n : 0n),
                '32-bit writes preserve high bank',
              );
              equal(result.state.registers[2], 42n, 'earlier load remains committed');
              equal(
                result.state.registers[3],
                high | (fault ? 0x1234n : 7n),
                'failing load publishes no destination',
              );
              equal(result.state.flags, fault ? 0x57n : 0x46n, 'exact prefix or last-lap flags');
              if (fault === 'segment-fault')
                equal(result.segmentFault?.segment, 'ds', 'checked DS has priority over page walk');
            },
          );
        }
      },
    },
    {
      name: 'paged 32-bit affine byte span keeps reads within a flat DS and the address ceiling',
      async run() {
        const head = 0x3fffd0n;
        const code = '0fb60731c90fb6040f0fb6540f010fb65c0f020fb6740f0301c583c10483f90875e3';
        const bytes = Uint8Array.of(0xff, 1, 2, 3, 4, 5, 6, 7);
        for (const layout of ['local', 'address-ceiling', 'wrap', 'nonflat'] as const) {
          const execution = context('compatibility');
          execution.segments.cs.base = 0x30n;
          if (layout === 'nonflat') execution.segments.ds.base = 0x400000n;
          const pdAt3 = new Uint8Array(0x1000);
          pdAt3.set(little([0x7027n]), 0xff8);
          const highPt = new Uint8Array(0x1000);
          highPt.set(little([0xb027n]), 0xff8);
          const data = {
            local: 0x401000n,
            'address-ceiling': 0xfffffff8n,
            wrap: 0xfffffffcn,
            nonflat: 0x1000n,
          }[layout];
          const spans =
            layout === 'wrap'
              ? [
                  { physical: 0x5000n, bytes: little([0xa027n]) },
                  { physical: 0xa000n, bytes: bytes.slice(4) },
                  { physical: 0xbffcn, bytes: bytes.slice(0, 4) },
                ]
              : [{ physical: layout === 'address-ceiling' ? 0xbff8n : 0xa000n, bytes }];
          await usingPaged(
            compiler,
            {
              code,
              context: execution,
              words: [
                [0x1000n, 0x2027n],
                [0x2000n, 0x3027n],
                [0x2018n, 0x6027n],
                [0x3000n, 0x5027n],
                [0x3010n, 0x4027n],
                [0x4000n, 0x9027n],
                [0x4008n, 0xa027n],
              ],
              spans: [
                { physical: 0x6000n, bytes: pdAt3 },
                { physical: 0x7000n, bytes: highPt },
                ...spans,
              ],
            },
            async (cpu) => {
              const high = 0x1234567800000000n;
              cpu.setState(
                initial(
                  head,
                  {
                    0: high,
                    1: high,
                    5: high,
                    6: high,
                    7: high | data,
                  },
                  2n,
                ),
              );
              const result = await cpu.run({ budget: 20n, stop: head + 34n });
              equal(result.reason, 'stop', layout);
              equal(result.retired, 18n, 'both laps retire');
              equal(result.state.rip, head + 34n, 'CS-relative exit');
              equal(result.state.registers[0], high | 4n, 'zero extension preserves high bank');
              equal(result.state.registers[1], high | 8n, 'induction keeps high bank');
              equal(result.state.registers[5], high | 259n, 'exact byte sum across the span');
              equal(result.state.registers[6], high | 7n, 'last byte is visible');
              equal(result.state.flags, 0x46n, 'the exit comparison supplies flags');
            },
          );
        }
      },
    },
    {
      name: 'paging review native cross-page fetch discards an owner invalidated by its one accessed-bit update',
      async run() {
        await usingPaged(
          compiler,
          {
            code: 'e9fb0f0000',
            words: [[0x4008n, 0xa007n]],
            spans: [{ physical: 0xa000n, bytes: Uint8Array.of(0x90, 0x82), execute: true }],
          },
          async (cpu) => {
            cpu.setState(initial(0x400000n));
            const result = await cpu.run({ budget: 2n });
            equal(result.reason, 'budget', 'JMP then one target NOP finish');
            equal(result.retired, 2n, 'completed JMP is not replayed');
            equal(result.state.rip, 0x401001n, 'target NOP commits once');
            equal(result.resources.pagingRestarts, 1, 'one changed A bit causes one abandonment');
            equal(result.resources.compilations, 2, 'invalidated owner is replaced');
            equal(result.resources.nativeEntries, 2, 'stale owner is never entered again');
          },
        );
      },
    },
    {
      name: 'paging review async compilation rejects stale code and root owners while public bank mutation stays busy',
      async run() {
        const create = Core.create;
        const compile = WebAssembly.compile;
        for (const root of [false, true]) {
          let observed: Core | undefined;
          Core.create = async (...args) => {
            observed = await create(...args);
            return observed;
          };
          const tables = new Uint8Array(0x4000);
          for (const [offset, value] of [
            [0, 0x6007n],
            [0x1000, 0x7007n],
            [0x2010, 0x8007n],
            [0x3000, 0xa007n],
          ] as const)
            tables.set(little([value]), offset);
          try {
            await usingPaged(
              compiler,
              {
                code: 'b801000000',
                spans: [
                  { physical: 0x5000n, bytes: tables },
                  { physical: 0xa000n, bytes: hex('b80200000082'), execute: true },
                ],
              },
              async (cpu) => {
                cpu.setState(initial(0x400000n));
                let changed = false;
                WebAssembly.compile = async (bytes) => {
                  if (!changed) {
                    changed = true;
                    check(observed, 'actual core captured');
                    let refused = false;
                    try {
                      cpu.setPagingState({ ...observed.readPagingState(), cr2: 0x1234n });
                    } catch {
                      refused = true;
                    }
                    check(refused, 'public paging mutation rejects while compiler is pending');
                    // Adversarial core mutation exercises the host's stale-install guard.
                    if (root)
                      observed.setPagingState({ ...observed.readPagingState(), cr3: 0x5000n });
                    else observed.writeOwnedMemory(0x9001n, hex('02000000'));
                  }
                  return compile(bytes);
                };
                try {
                  const result = await cpu.run({ budget: 1n });
                  equal(result.reason, 'budget', 'replacement code completes');
                  equal(result.retired, 1n, 'old source never executes');
                  equal(
                    result.state.registers[0],
                    2n,
                    root ? 'new root owns code' : 'patched immediate owns code',
                  );
                  equal(
                    result.resources.compilations,
                    2,
                    'one rejected install and one current install',
                  );
                  equal(result.resources.nativeEntries, 1, 'only current module enters');
                  equal(
                    cpu.readPagingState().cr3,
                    root ? 0x5000n : 0x1000n,
                    'current translation authority retained',
                  );
                } finally {
                  WebAssembly.compile = compile;
                }
              },
            );
          } finally {
            Core.create = create;
            WebAssembly.compile = compile;
          }
        }
      },
    },
    {
      name: 'paging review real bank and mapping getter reentry preserves nested repairs and frozen PF state',
      async run() {
        await usingPaged(compiler, {}, async (cpu) => {
          const event = {
            kind: 'pending' as const,
            exception: {
              vector: 14 as const,
              instructionPc: 0x3ffff0n,
              returnPc: 0x400000n,
              savedFlags: 0x10202n,
              errorCode: 6,
              pageFaultAddress: 0x501000n,
            },
          };
          cpu.setState({ ...initial(0x400000n, {}, 0x202n), event });
          const bank = cpu.readPagingState();
          let nested: ReturnType<typeof cpu.run> | undefined;
          let reads = 0;
          let rejected = false;
          try {
            cpu.setPagingState({
              ...bank,
              get cr2() {
                reads++;
                nested = cpu.run({ budget: 0n });
                return 0x1234n;
              },
            });
          } catch {
            rejected = true;
          }
          check(rejected, 'completed nested run rejects outer capture by generation');
          equal(reads, 1, 'bank getter captured once');
          await nested;
          same(cpu.readPagingState(), bank, 'no outer bank publication');
          same(cpu.readState().event, event, 'nested budget gate leaves frozen source');
          rejected = false;
          try {
            cpu.setPagingState({
              ...bank,
              cr0: 0n,
              get cr2() {
                cpu.setPagingState({ ...bank, cr2: 0xdeadn });
                return 0n;
              },
            });
          } catch {
            rejected = true;
          }
          check(rejected, 'invalid outer bank rejected');
          equal(cpu.readPagingState().cr2, 0xdeadn, 'nested successful bank repair retained');
          same(cpu.readState().event, event, 'bank repair cannot cancel selected PF');
          rejected = false;
          try {
            cpu.setMappings([
              {
                base: 1n << 52n,
                backing: 'tables',
                offset: 0,
                length: 1,
                get permissions() {
                  cpu.setPagingState({ ...bank, cr2: 0xbeefn });
                  return { read: true, write: true, execute: false };
                },
              },
            ]);
          } catch {
            rejected = true;
          }
          check(rejected, 'invalid physical mapping rejected');
          equal(cpu.readPagingState().cr2, 0xbeefn, 'mapping getter nested repair retained');
          equal(
            word(cpu, 0x4000n),
            0x9007n,
            'failed mapping capture did not replace memory or walk',
          );
          same(cpu.readState().event, event, 'failed mapping capture retained PF');
          let touched = false;
          const poison = new Proxy(new Uint8Array(1), {
            get(target, key, receiver) {
              touched = true;
              return Reflect.get(target, key, receiver);
            },
          });
          rejected = false;
          try {
            cpu.writeMemory(0x1000n, poison);
          } catch {
            rejected = true;
          }
          check(rejected && !touched, 'wrong-plane operation rejects before inspecting bytes');
        });
      },
    },
  ];
}
