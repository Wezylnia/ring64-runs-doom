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
      name: 'warm shared paged reads preserve split bytes and exact physical read refusal',
      async run() {
        const options = pagedOptions(compiler, '488b03', [0xa007n, 0xb007n]);
        const data = PC + 0x1000n;
        const deniedStart = span(0xa080n, 'old', 0x80, 8);
        options.memory.mappings.splice(
          2,
          1,
          span(0xa000n, 'old', 0, 0x80),
          { ...deniedStart, permissions: { ...deniedStart.permissions, read: false } },
          span(0xa088n, 'old', 0x88, 0xf78),
        );
        const [, , old, next] = options.memory.backings;
        old!.bytes.set(asBytes(0x8877665544332211n), 0x3d);
        old!.bytes.set(Uint8Array.of(0x01, 0x23, 0x45), 0xffd);
        next!.bytes.set(Uint8Array.of(0x67, 0x89, 0xab, 0xcd, 0xef));
        await withMachine(factory, options, async (machine) => {
          const [reader, peer] = machine.cpus;
          const read = async (cpu: TestCpu, address: bigint) => {
            await cpu.setState(cpuState(PC, { 0: 0xabadcafen, 3: address }));
            return cpu.run({ budget: 2n, stop: PC + 3n });
          };
          const load = async (cpu: TestCpu, address: bigint, want: bigint, label: string) => {
            const result = await read(cpu, address);
            done(result, PC + 3n, 1n);
            equal(result.state.registers[0], want, label);
          };
          await load(reader!, data, 0n, 'first page admitted');
          await load(reader!, data + 0x1000n, 0xefcdab8967n, 'second page admitted');
          await load(reader!, data + 0x3dn, 0x8877665544332211n, 'warm read crosses a stripe');
          await load(reader!, data + 0xffdn, 0xefcdab8967452301n, 'warm read crosses guest pages');
          const denied = await read(reader!, data + 0x7dn);
          equal(denied.reason, 'physical-memory-fault', 'whole operand refused by the tail');
          equal(denied.retired, 0n, 'faulting read is unretired');
          equal(denied.state.rip, PC, 'faulting instruction retained');
          equal(denied.state.registers[0], 0xabadcafen, 'read publishes no partial register');
          same(
            denied.physicalMemoryFault,
            {
              instructionPc: PC,
              address: data + 0x7dn,
              access: 'read',
              physicalAddress: 0xa07dn,
              width: 8,
              failingPhysicalAddress: 0xa080n,
              physicalAccess: 'read',
              cause: 'permission',
              phase: 'final-access',
              segment: 'ds',
            },
            'exact physical read failure',
          );
          await load(peer!, data + 0x3dn, 0x8877665544332211n, 'peer independently reads RAM');
        });
      },
    },
    {
      name: 'warm shared paged MOVSS stays coherent across canonical lanes through an alias',
      async run() {
        const writerCode = '488703ffc975f9'; // XCHG [RBX],RAX; DEC ECX; JNZ writer.
        const readerCode =
          'f30f1003660f7ec0' + // MOVSS XMM0,[RBX]; MOVD EAX,XMM0
          '39f0740439f87505' + // CMP EAX,ESI; JE next; CMP EAX,EDI; JNE bad
          'ffc975ec900f0b'; // next: DEC ECX; JNZ reader; NOP; bad: UD2
        const reader = PC + BigInt(writerCode.length / 2);
        const stop = reader + 20n;
        const readerData = PC + 0x1005n;
        const writerData = PC + 0x2005n;
        const low = 0x7766554433221100n;
        const high = 0xcafedecab00badc0n;
        const options = pagedOptions(compiler, writerCode + readerCode, [0xa007n, 0xa007n]);
        options.memory.backings[2]!.bytes.set(asBytes(low), 5);
        const readState = (count: bigint) =>
          cpuState(reader, {
            1: count,
            3: readerData,
            6: low & 0xffffffffn,
            7: high & 0xffffffffn,
          });
        await withMachine(factory, options, async (machine) => {
          const [writer, observer] = machine.cpus;
          await observer!.setState(readState(1n));
          done(await observer!.run({ budget: 9n, stop }), stop, 6n);
          await writer!.setState(cpuState(PC, { 0: high, 1: 2n, 3: writerData }));
          done(await writer!.run({ budget: 7n, stop: reader }), reader, 6n);

          const count = 1024n;
          await writer!.setState(cpuState(PC, { 0: high, 1: count, 3: writerData }));
          await observer!.setState(readState(count));
          const [w, r] = await Promise.all([
            writer!.run({ budget: count * 3n + 1n, stop: reader }),
            observer!.run({ budget: count * 8n + 1n, stop }),
          ]);
          done(w, reader, count * 3n);
          done(r, stop);
          equal(r.state.registers[1], 0n, 'every aliased MOVSS returned a complete dword');
          equal(await physicalWord(machine, 0xa005n), low, 'even swaps restore the original qword');
          equal(w.state.registers[0], high, 'writer keeps the other complete qword');
        });
      },
    },
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
