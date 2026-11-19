import { RESET_BANK, resetContext } from '../reset/fixture.js';
import type { Scenario } from '../../support/fixture.js';
import {
  ENTRY,
  RAM,
  asBytes,
  cpuState,
  done,
  equal,
  hex,
  linearMachine,
  same,
  span,
  withMachine,
  words,
  type MachineFactory,
} from './fixture.js';

export function parallelLifecycleScenarios(
  factory: MachineFactory,
  compiler: WebAssembly.Module,
): Scenario[] {
  return [
    {
      name: 'parallel pause preserves in-flight private register progress while the peer finishes',
      async run() {
        // A publishes loop entry and then increments RAX while gate is zero. B waits for A
        // before publishing its distinct result, so the host requests a demonstrated active pause.
        const code = new Uint8Array(64).fill(0x90);
        code.set([
          0x90, 0x90, 0xc7, 0x07, 1, 0, 0, 0, 0x48, 0xff, 0xc0, 0x83, 0x7f, 4, 0, 0x74, 0xf7,
        ]);
        code.set([0x83, 0x3f, 0, 0x74, 0xfb, 0xc7, 0x47, 8, 7, 0, 0, 0], 32);
        await withMachine(factory, linearMachine(compiler, code), async (machine) => {
          const [busy, peer] = machine.cpus;
          await busy!.setState(cpuState(ENTRY, { 0: 0n, 7: RAM }));
          await peer!.setState(cpuState(ENTRY + 32n, { 7: RAM }));
          const background = busy!.run({ budget: 10_000_000_000n, stop: ENTRY + 17n });
          const other = await peer!.run({ budget: 2_000_000n, stop: ENTRY + 44n });
          done(other, ENTRY + 44n);
          await busy!.pause();
          const paused = await background;
          equal(paused.reason, 'paused', 'active run reports an acknowledged pause');
          equal(paused.retired > 1n, true, 'reached progress before pause');
          equal(
            paused.remaining,
            10_000_000_000n - paused.retired,
            'pause does not charge a synthetic retirement',
          );
          same(await words(machine, RAM, 2), [1n, 7n], 'peer outcome actually published');
          const before = paused.state.registers[0]!;
          await machine.writeMemory(RAM, asBytes(0x100000001n));
          const resumed = await busy!.run({ budget: 10n, stop: ENTRY + 17n });
          done(resumed, ENTRY + 17n);
          const after = resumed.state.registers[0]!;
          equal(
            after === before || after === before + 1n,
            true,
            'resumes saved instruction, without replaying loop prologue',
          );
        });
      },
    },
    {
      name: 'parallel physical reset keeps RAM, resets timing and distinguishes local from cohort reset',
      async run() {
        const reset = 0xfff0n,
          stop = reset + 3n;
        const options = linearMachine(compiler, '90');
        options.memoryModel = 'physical';
        options.memory = {
          backings: [
            { id: 'rom', bytes: hex('b8341290') },
            { id: 'ram', bytes: new Uint8Array(64) },
          ],
          mappings: [span(0xfffffff0n, 'rom', 0, 4, 'rx'), span(0n, 'ram', 0, 64)],
        };
        options.cpus = options.cpus.map(() => ({
          entry: reset,
          context: resetContext(),
          paging: RESET_BANK,
        }));
        await withMachine(factory, options, async (machine) => {
          const [a, b] = machine.cpus;
          await machine.writePhysicalMemory(8n, Uint8Array.of(0x7b));
          const runs = await Promise.all(machine.cpus.map((cpu) => cpu.run({ budget: 2n, stop })));
          for (const result of runs) {
            done(result, stop, 1n);
            equal(result.state.registers[0]! & 0xffffn, 0x1234n, 'real-mode reset vector executed');
          }
          await a!.reset();
          equal((await a!.readState()).rip, reset, 'first CPU resets');
          equal((await a!.readTimeState()).tsc, 0n, 'local clock resets');
          equal((await b!.readState()).rip, stop, 'peer architectural state intact');
          equal((await b!.readTimeState()).tsc, 1n, 'peer clock intact');
          await machine.reset();
          for (const cpu of machine.cpus) {
            equal((await cpu.readState()).rip, reset, 'cohort returns to reset vector');
            equal((await cpu.readTimeState()).tsc, 0n, 'cohort reset updates local clock');
          }
          equal((await machine.readPhysicalMemory(8n, 1))[0], 0x7b, 'physical backing preserved');
        });
      },
    },
    {
      name: 'parallel REP preserves completed elements through a late write fault and one repaired retirement',
      async run() {
        const options = linearMachine(compiler, 'f3a490');
        const source = RAM,
          dest = RAM + 0x100n;
        options.memory.backings[1]!.bytes.set([0x11, 0x22, 0x33, 0x44]);
        options.memory.backings[1]!.bytes.fill(0xa5, 64, 68);
        options.memory.mappings.splice(
          1,
          1,
          span(source, 'data', 0, 4),
          span(dest, 'data', 64, 2),
          span(dest + 2n, 'data', 66, 2, 'r'),
        );
        await withMachine(factory, options, async (machine) => {
          const cpu = machine.cpus[0]!;
          const stop = ENTRY + 2n;
          await cpu.setState(cpuState(ENTRY, { 1: 4n, 6: source, 7: dest }));
          const fault = await cpu.run({ budget: 2n, repeatBudget: 4n, stop });
          equal(fault.reason, 'data-fault', 'third store failed');
          equal(fault.fault?.failingAddress, dest + 2n, 'reached failing byte');
          equal(fault.retired, 0n, 'partial REP has not retired');
          equal(fault.repeatIterations, 2n, 'committed iterations counted');
          equal(fault.state.rip, ENTRY, 'restarts at REP');
          equal(fault.state.registers[1], 2n, 'remaining count');
          equal(fault.state.registers[6], source + 2n, 'advanced source');
          equal(fault.state.registers[7], dest + 2n, 'advanced destination');
          same(
            [...(await machine.readMemory(dest, 4))],
            [0x11, 0x22, 0xa5, 0xa5],
            'partial memory effects',
          );
          await machine.setMappings(
            options.memory.mappings.map((m) =>
              m.base === dest + 2n ? span(dest + 2n, 'data', 66, 2) : m,
            ),
          );
          const complete = await cpu.run({ budget: 2n, repeatBudget: 4n, stop });
          done(complete, stop, 1n);
          equal(complete.repeatIterations, 2n, 'retry publishes only the remaining elements');
          same(
            [...(await machine.readMemory(dest, 4))],
            [0x11, 0x22, 0x33, 0x44],
            'copy completed exactly',
          );
        });
      },
    },
    {
      name: 'a parallel data fault terminates only the targeted guest and preserves peer writes',
      async run() {
        // mov eax,[rdi]; inc eax; mov [rdi],eax.
        await withMachine(factory, linearMachine(compiler, '8b07ffc08907'), async (machine) => {
          const [faulting, peer] = machine.cpus;
          await faulting!.setState(cpuState(ENTRY, { 0: 12n, 7: RAM - 1n }));
          await peer!.setState(cpuState(ENTRY, { 0: 34n, 7: RAM }));
          const [bad, good] = await Promise.all(
            machine.cpus.map((cpu) => cpu.run({ budget: 4n, stop: ENTRY + 6n })),
          );
          equal(bad!.reason, 'data-fault', 'guest-facing fault');
          equal(bad!.retired, 0n, 'faulting read does not retire');
          equal(bad!.state.rip, ENTRY, 'instruction retained for peer-independent repair');
          equal(bad!.state.registers[0], 12n, 'faulting partial span changes no register');
          equal(bad!.fault?.failingAddress, RAM - 1n, 'start of absent span');
          done(good!, ENTRY + 6n, 3n);
          same(await words(machine, RAM, 1), [1n], 'peer store is observable');
          await faulting!.setState(cpuState(ENTRY, { 7: RAM }));
          done(await faulting!.run({ budget: 4n, stop: ENTRY + 6n }), ENTRY + 6n, 3n);
          same(await words(machine, RAM, 1), [2n], 'retry completes once');
        });
      },
    },
  ];
}
