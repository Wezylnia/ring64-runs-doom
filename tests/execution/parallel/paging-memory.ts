import { BANK, PC, little } from '../paging/fixture.js';
import type { Scenario } from '../../support/fixture.js';
import {
  asBytes,
  cpuState,
  done,
  equal,
  hex,
  linearMachine,
  same,
  span,
  withMachine,
  type MachineFactory,
  type TestCpu,
  type TestMachine,
} from './fixture.js';

function pagedOptions(compiler: WebAssembly.Module, code: string, leaves: readonly bigint[]) {
  const options = linearMachine(compiler, code);
  const tables = new Uint8Array(0x4000);
  tables.set(little([0x2007n]));
  tables.set(little([0x3007n]), 0x1000);
  tables.set(little([0x4007n]), 0x2010);
  tables.set(little([0x9007n, ...leaves]), 0x3000);
  const program = hex(code);
  options.memoryModel = 'ia32e-paged';
  options.cpus = options.cpus.map(() => ({
    entry: PC,
    paging: { ...BANK, cr0: BANK.cr0 | 0x10000n },
  }));
  options.memory = {
    backings: [
      { id: 'tables', bytes: tables },
      { id: 'code', bytes: program },
      { id: 'old', bytes: new Uint8Array(4096) },
      { id: 'new', bytes: new Uint8Array(4096) },
    ],
    mappings: [
      span(0x1000n, 'tables', 0, tables.length),
      span(0x9000n, 'code', 0, program.length, 'rx'),
      span(0xa000n, 'old', 0, 4096),
      span(0xb000n, 'new', 0, 4096),
    ],
  };
  return options;
}

async function physicalWord(machine: TestMachine, address: bigint): Promise<bigint> {
  const bytes = await machine.readPhysicalMemory(address, 8);
  return new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength).getBigUint64(0, true);
}

export function parallelPagingMemoryScenarios(
  factory: MachineFactory,
  compiler: WebAssembly.Module,
): Scenario[] {
  return [
    {
      name: 'guest PTE replacement preserves both local TLBs until the selected CPU executes INVLPG',
      async run() {
        // mov rax,[rbx]; mov [rcx],rdx; invlpg [rbx]. The third VA aliases the PTE page.
        const options = pagedOptions(compiler, '488b034889110f013b', [0xa007n, 0x4007n]);
        const data = PC + 0x1000n;
        await withMachine(factory, options, async (machine) => {
          const [a, b] = machine.cpus;
          await machine.writePhysicalMemory(0xa000n, asBytes(0x1122334455667788n));
          await machine.writePhysicalMemory(0xb000n, asBytes(0x8877665544332211n));
          const load = async (cpu: TestCpu, want: bigint, label: string) => {
            await cpu.setState(cpuState(PC, { 3: data }));
            const result = await cpu.run({ budget: 2n, stop: PC + 3n });
            done(result, PC + 3n, 1n);
            equal(result.state.registers[0], want, label);
          };
          await Promise.all([
            load(a!, 0x1122334455667788n, 'CPU A fills its own TLB'),
            load(b!, 0x1122334455667788n, 'CPU B fills its own TLB'),
          ]);
          equal(await physicalWord(machine, 0x4008n), 0xa027n, 'old leaf was accessed');
          await a!.setState(cpuState(PC + 3n, { 1: PC + 0x2008n, 2: 0xb007n }));
          done(await a!.run({ budget: 2n, stop: PC + 6n }), PC + 6n, 1n);
          equal(await physicalWord(machine, 0x4008n), 0xb007n, 'reached guest PTE store');
          await Promise.all([
            load(a!, 0x1122334455667788n, 'writer keeps its old TLB translation'),
            load(b!, 0x1122334455667788n, 'peer keeps its old TLB translation'),
          ]);
          equal(
            await physicalWord(machine, 0x4008n),
            0xb007n,
            'cached old reads did not reach the replacement leaf',
          );
          await b!.setState(cpuState(PC + 6n, { 3: data }));
          done(await b!.run({ budget: 2n, stop: PC + 9n }), PC + 9n, 1n);
          await Promise.all([
            load(a!, 0x1122334455667788n, 'peer INVLPG leaves the writer translation intact'),
            load(b!, 0x8877665544332211n, 'local INVLPG now observes the replacement'),
          ]);
          equal(
            await physicalWord(machine, 0x4008n),
            0xb027n,
            'only fresh read reaches the new leaf',
          );
          await machine.writePhysicalMemory(0x4008n, asBytes(0xa007n));
          await Promise.all([
            load(a!, 0x1122334455667788n, 'administrative edit first CPU'),
            load(b!, 0x1122334455667788n, 'administrative edit flushes the second CPU'),
          ]);
        });
      },
    },
    {
      name: 'paged split write denial preserves the first payload and dirty bit until repair',
      async run() {
        // mov [rbx],rax across two virtual pages with distinct physical backings.
        const options = pagedOptions(compiler, '48890390', [0xa007n, 0xb005n]);
        const first = PC + 0x1ffdn;
        const payload = asBytes(0x8877665544332211n);
        await withMachine(factory, options, async (machine) => {
          const [writer, peer] = machine.cpus;
          await machine.writePhysicalMemory(0xaffdn, Uint8Array.of(0xaa, 0xbb, 0xcc));
          await writer!.setState(cpuState(PC, { 0: 0x8877665544332211n, 3: first }));
          const denied = await writer!.run({ budget: 2n, stop: PC + 3n });
          equal(denied.reason, 'page-fault', 'late write denial');
          equal(denied.retired, 0n, 'whole scalar store is unretired');
          equal(denied.state.rip, PC, 'fault instruction retained');
          same(
            denied.pageFault,
            {
              vector: 14,
              instructionPc: PC,
              address: PC + 0x2000n,
              errorCode: 3,
              access: 'write',
            },
            'late tail fault',
          );
          equal((await writer!.readPagingState()).cr2, PC + 0x2000n, 'fault address in the writer');
          equal((await peer!.readPagingState()).cr2, 0n, 'peer CR2 is private');
          equal((await physicalWord(machine, 0x4008n)) & 0x40n, 0n, 'first leaf not dirty');
          equal((await physicalWord(machine, 0x4010n)) & 0x40n, 0n, 'denied leaf not dirty');
          same(
            [...(await machine.readPhysicalMemory(0xaffdn, 3))],
            [0xaa, 0xbb, 0xcc],
            'writable prefix untouched',
          );
          same(
            [...(await machine.readPhysicalMemory(0xb000n, 5))],
            [0, 0, 0, 0, 0],
            'denied suffix untouched',
          );
          await machine.writePhysicalMemory(0x4010n, asBytes(0xb007n));
          done(await writer!.run({ budget: 2n, stop: PC + 3n }), PC + 3n, 1n);
          equal(await physicalWord(machine, 0x4008n), 0xa067n, 'first leaf accessed and dirty');
          equal(await physicalWord(machine, 0x4010n), 0xb067n, 'second leaf accessed and dirty');
          same(
            [
              ...(await machine.readPhysicalMemory(0xaffdn, 3)),
              ...(await machine.readPhysicalMemory(0xb000n, 5)),
            ],
            [...payload],
            'full repaired store',
          );
        });
      },
    },
  ];
}
