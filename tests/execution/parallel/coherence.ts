import { snapshot, loadedCode, loadedData } from '../far-control/fixture.js';
import { BANK, PC, little } from '../paging/fixture.js';
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

export function parallelCoherenceScenarios(
  factory: MachineFactory,
  compiler: WebAssembly.Module,
): Scenario[] {
  return [
    {
      name: 'a warm store loop rechecks write permission after a live mapping replacement',
      async run() {
        // mov dword [rdi],1; out 0x80,eax; jmp back to the same store
        const options = linearMachine(compiler, 'c70701000000e780ebf6');
        let writes = 0;
        let ready!: () => void;
        const reached = new Promise<void>((resolve) => {
          ready = resolve;
        });
        options.io = {
          read: () => 0,
          write(port, width) {
            equal(port, 0x80, 'reached port');
            equal(width, 32, 'OUT operand width');
            if (++writes === 2) ready();
          },
        };
        await withMachine(factory, options, async (machine) => {
          const cpu = machine.cpus[0]!;
          await cpu.setState(cpuState(ENTRY, { 7: RAM }));
          const running = cpu.run({ budget: 20_000_000n });
          await Promise.race([
            reached,
            running.then(() => {
              throw new Error('the warmed store loop exited before remap');
            }),
          ]);
          await machine.setMappings([options.memory.mappings[0]!, span(RAM, 'data', 0, 4096, 'r')]);
          const fault = await running;
          equal(fault.reason, 'data-fault', 'the next store is checked in the replacement map');
          equal(fault.state.rip, ENTRY, 'faulting store did not retire');
          equal(fault.fault?.access, 'write', 'write admission');
          equal(fault.fault?.cause, 'permission', 'new read-only permissions');
          equal(fault.fault?.failingAddress, RAM, 'precise address');
          equal((await machine.readMemory(RAM, 4))[0], 1, 'the earlier reached store committed');
        });
      },
    },
    {
      name: 'peer code patches through RW alias displace a warm executable owner before CPUID successor',
      async run() {
        // Reader at pc: entered=1; wait: cmp [rdi],0; je wait; xor eax,eax; cpuid;
        // jmp target; target: mov eax,1; mov [rsi],eax; ret.
        const code = new Uint8Array(96).fill(0x90);
        code.set(hex('c7470401000000833f0074fb31c00fa2eb1e'));
        code.set(hex('b8010000008906c3'), 48);
        // Writer: wait: cmp [rsi+4],0; je wait; mov byte [rdi],2; mov dword [rsi],1.
        const writerCode = hex('837e040074fac60702c70601000000');
        code.set(writerCode, 64);
        const alias = 0x8000n;
        const options = linearMachine(compiler, code);
        options.memory.mappings.push(span(alias, 'code', 0, code.length));
        await withMachine(factory, options, async (machine) => {
          const [reader, writer] = machine.cpus;
          const patch = ENTRY + 48n;
          const doneAt = patch + 7n;
          await reader!.setState(cpuState(patch, { 6: RAM + 16n }));
          const warmed = await reader!.run({ budget: 3n, stop: doneAt });
          done(warmed, doneAt, 2n);
          same(await words(machine, RAM + 16n, 1), [1n], 'old genuine target');

          // Same backing and page, outside every accepted source or data dependency.
          await machine.writeMemory(alias + 95n, Uint8Array.of(0xcc));
          await reader!.setState(cpuState(patch, { 6: RAM + 16n }));
          const unaffected = await reader!.run({ budget: 3n, stop: doneAt });
          done(unaffected, doneAt, 2n);
          equal(unaffected.resources.compilations, 0, 'neighboring byte preserves warm owner');
          await machine.writeMemory(RAM + 16n, asBytes(0n));

          await reader!.setState(cpuState(ENTRY, { 6: RAM + 16n, 7: RAM }));
          await writer!.setState(cpuState(ENTRY + 64n, { 6: RAM, 7: alias + 49n }));
          const [r, w] = await Promise.all([
            reader!.run({ budget: 2_000_000n, stop: doneAt }),
            writer!.run({ budget: 2_000_000n, stop: ENTRY + 64n + BigInt(writerCode.length) }),
          ]);
          done(r, doneAt);
          done(w, ENTRY + 64n + BigInt(writerCode.length));
          equal(r.state.registers[0], 2n, 'CPUID successor uses the new executable bytes');
          equal((await machine.readMemory(patch + 1n, 1))[0], 2, 'RX and RW aliases share bytes');
          same(
            await words(machine, RAM, 3),
            [0x100000001n, 0n, 2n],
            'both handoffs and new result',
          );
        });
      },
    },
    {
      name: 'shared paged decode reaches byte 15 before #GP and repairs a peer-independent split fetch',
      async run() {
        const end = PC + 0x1000n,
          first = 0x9000n;
        const firstPage = new Uint8Array(4096).fill(0x90);
        firstPage[4095] = 0x0f;
        const tail = new Uint8Array(4096).fill(0x90);
        tail[0] = 0xa2;
        const table = new Uint8Array(0x4000);
        for (const [offset, value] of [
          [0n, 0x2007n],
          [0x1000n, 0x3007n],
          [0x2010n, 0x4007n],
          [0x3000n, 0x9007n],
        ] as const)
          table.set(little([value]), Number(offset));
        const options = linearMachine(compiler, '90');
        options.memoryModel = 'ia32e-paged';
        options.memory = {
          backings: [
            { id: 'tables', bytes: table },
            { id: 'first', bytes: firstPage },
            { id: 'last', bytes: tail },
          ],
          mappings: [
            span(0x1000n, 'tables', 0, table.length),
            span(first, 'first', 0, firstPage.length, 'rwx'),
            span(0xa000n, 'last', 0, tail.length, 'rwx'),
          ],
        };
        options.cpus = [
          { entry: end - 1n, paging: { ...BANK, efer: BANK.efer | 0x800n } },
          { entry: PC, paging: { ...BANK, efer: BANK.efer | 0x800n } },
        ];
        await withMachine(factory, options, async (machine) => {
          const [fetching, peer] = machine.cpus;
          const [faulted, other] = await Promise.all([
            fetching!.run({ budget: 2n, stop: end + 1n }),
            peer!.run({ budget: 2n, stop: PC + 1n }),
          ]);
          done(other, PC + 1n, 1n);
          equal(faulted.reason, 'page-fault', 'missing fetched selector');
          equal(faulted.retired, 0n, 'no split fetch retirement');
          same(
            faulted.pageFault,
            {
              vector: 14,
              instructionPc: end - 1n,
              address: end,
              errorCode: 16,
              access: 'execute',
            },
            'exact split fault',
          );
          equal((await fetching!.readPagingState()).cr2, end, 'targeted CR2');
          equal((await peer!.readPagingState()).cr2, 0n, 'other CR2 unaffected');
          await machine.writePhysicalMemory(first + 4093n, hex('f00fa2'));
          await fetching!.setState(cpuState(end - 3n));
          const illegal = await fetching!.run({ budget: 1n });
          equal(illegal.reason, 'invalid-encoding', 'complete LOCK CPUID gives #UD');
          equal(illegal.retired, 0n, 'LOCK legality precedes CPUID');
          equal(illegal.pageFault, undefined, 'complete encoding did not fetch its successor');
          await machine.writePhysicalMemory(first + 4081n, new Uint8Array(15).fill(0x66));
          await fetching!.setState(cpuState(end - 15n));
          const tooLong = await fetching!.run({ budget: 1n });
          equal(tooLong.reason, 'invalid-encoding', 'required byte 16 rejected as #GP');
          equal(tooLong.retired, 0n, 'no retirement on length rejection');
          equal(tooLong.pageFault, undefined, 'byte 16 was not fetched');
          equal((await fetching!.readPagingState()).cr2, end, 'length check preserves CR2');
          await machine.writePhysicalMemory(first + 4095n, Uint8Array.of(0x0f));
          await machine.writePhysicalMemory(0x4008n, little([0xa007n]));
          await fetching!.setState(cpuState(end - 1n, { 0: 0n, 1: 0n }));
          const repaired = await fetching!.run({ budget: 2n, stop: end + 1n });
          done(repaired, end + 1n, 1n);
          equal(repaired.state.registers[0]! > 0n, true, 'repaired split CPUID ran');
          const updated = await machine.readPhysicalMemory(0x4008n, 8);
          equal(
            new DataView(updated.buffer, updated.byteOffset).getBigUint64(0, true),
            0xa027n,
            'reached selector fetch sets accessed',
          );
        });
      },
    },
    {
      name: 'a suppressed paged x87 store drops its unused write grant at the reached boundary',
      async run() {
        const table = new Uint8Array(0x4000);
        for (const [offset, value] of [
          [0, 0x2007n],
          [0x1000, 0x3007n],
          [0x2010, 0x4007n],
          [0x3000, 0x9007n],
          [0x3008, 0x4027n],
        ] as const)
          table.set(little([value]), offset);
        const options = linearMachine(compiler, 'db1f');
        options.memoryModel = 'ia32e-paged';
        options.cpus = [
          { entry: PC, paging: BANK },
          { entry: PC, paging: BANK },
        ];
        options.memory = {
          backings: [
            { id: 'tables', bytes: table },
            { id: 'code', bytes: hex('db1f') },
          ],
          mappings: [span(0x1000n, 'tables', 0, table.length), span(0x9000n, 'code', 0, 2, 'rx')],
        };
        await withMachine(factory, options, async (machine) => {
          const cpu = machine.cpus[0]!;
          await cpu.setState(cpuState(PC, { 7: PC + 0x1008n }));
          await cpu.setX87State({ ...(await cpu.readX87State()), controlWord: 0x037e });
          const result = await cpu.run({ budget: 1n });
          equal(result.reason, 'budget', 'suppressed FISTP completes');
          equal(result.retired, 1n, 'one retirement');
          equal(result.state.rip, PC + 2n, 'recorded completion boundary');
          equal((await cpu.readX87State()).statusWord & 1, 1, 'invalid operation remains pending');
          same(
            [...(await machine.readPhysicalMemory(0x4008n, 8))],
            [...little([0x4027n])],
            'the suppressed store publishes neither its overlapping bytes nor D',
          );
        });
      },
    },
    {
      name: 'parallel legacy walkers merge A/D in adjacent PTEs of one shared backing lane',
      async run() {
        const at = 0x402000n;
        const bytes = new Uint8Array(0x10000);
        bytes.set(hex('890790'), 0x9000);
        bytes.set(little([0x2003n], 4), 0x1004);
        bytes.set(little([0x6003n, 0x7003n, 0x9003n], 4), 0x2000);
        const protected32 = snapshot('protected');
        protected32.segments.cs = loadedCode(32);
        protected32.segments.ss = loadedData(32);
        const paging = { cr0: 0x80000033n, cr2: 0n, cr3: 0x1000n, cr4: 0x600n, efer: 0n };
        const options = linearMachine(compiler, '90');
        options.memoryModel = 'physical';
        options.memory = {
          backings: [{ id: 'ram', bytes }],
          mappings: [span(0n, 'ram', 0, bytes.length, 'rwx')],
        };
        options.cpus = options.cpus.map(() => ({ entry: at, context: protected32, paging }));
        await withMachine(factory, options, async (machine) => {
          for (const [i, cpu] of machine.cpus.entries()) {
            await cpu.setState(
              cpuState(at, { 0: BigInt(0x20 + i), 7: 0x400000n + BigInt(i) * 0x1000n }),
            );
          }
          const stops = await Promise.all(
            machine.cpus.map((cpu) => cpu.run({ budget: 2n, stop: at + 2n })),
          );
          for (const result of stops) done(result, at + 2n, 1n);
          const ptes = await machine.readPhysicalMemory(0x2000n, 12);
          same(
            [...ptes],
            [...little([0x6063n, 0x7063n, 0x9023n], 4)],
            'independent shared PTE A/D and code A bits',
          );
          equal((await machine.readPhysicalMemory(0x6000n, 1))[0], 0x20, 'first payload visible');
          equal((await machine.readPhysicalMemory(0x7000n, 1))[0], 0x21, 'second payload visible');
        });
      },
    },
    {
      name: 'a host map replacement revokes a retained middle entry and keeps unrelated peers runnable',
      async run() {
        // Two separately sourced bytes of the same body: nop; mov eax,1; nop.
        const options = linearMachine(compiler, '90b80100000090');
        options.memory.backings.push({ id: 'replacement', bytes: hex('90b80200000090') });
        await withMachine(factory, options, async (machine) => {
          const [a, b] = machine.cpus;
          const stop = ENTRY + 6n;
          for (const cpu of machine.cpus) {
            const r = await cpu.run({ budget: 4n, stop });
            done(r, stop, 2n);
            equal(r.state.registers[0], 1n, 'each worker compiles old mapping');
          }
          await machine.setMappings(
            options.memory.mappings.map((m) =>
              m.base === ENTRY ? { ...m, backing: 'replacement' } : m,
            ),
          );
          await Promise.all(machine.cpus.map((cpu) => cpu.setState(cpuState(ENTRY + 1n))));
          const results = await Promise.all([
            a!.run({ budget: 2n, stop }),
            b!.run({ budget: 2n, stop }),
          ]);
          for (const r of results) {
            done(r, stop, 1n);
            equal(r.state.registers[0], 2n, 'new mapping at prior middle entry');
          }
          equal((await machine.readMemory(ENTRY + 2n, 1))[0], 2, 'published map visible to host');
          await machine.removeBacking('code');
          await machine.addBacking({ id: 'code', bytes: hex('90b80300000090') });
          await machine.setMappings(options.memory.mappings);
          await a!.setState(cpuState(ENTRY + 1n));
          const rebound = await a!.run({ budget: 2n, stop });
          done(rebound, stop, 1n);
          equal(rebound.state.registers[0], 3n, 'backing ID reuse creates a fresh incarnation');
        });
      },
    },
  ];
}
