import { context } from '../../../modes/fixture.js';
import { BANK, equal, initial, little, same, usingPaged, type Scenario } from '../fixture.js';

export function indexedReadLoopScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
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
  ];
}
