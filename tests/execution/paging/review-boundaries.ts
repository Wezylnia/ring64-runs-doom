import { Core } from '../../../sdk/src/wasm/core.js';
import {
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

export function pagingBoundaryReviewScenarios(compiler: WebAssembly.Module): Scenario[] {
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
