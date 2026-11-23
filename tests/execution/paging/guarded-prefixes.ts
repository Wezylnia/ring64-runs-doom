import { observeArtifactImports } from '../../support/artifact-imports.js';
import { equal, initial, little, same, usingPaged, word, type Scenario } from './fixture.js';

export function guardedPrefixScenarios(compiler: WebAssembly.Module): Scenario[] {
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
  ];
}
