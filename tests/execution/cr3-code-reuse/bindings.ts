import {
  at,
  CODE_A,
  CODE_B,
  equal,
  hex,
  NEXT_A,
  PC,
  ROOT_A,
  ROOT_B,
  SHARED,
  SHARED_PC,
  switchRoot,
  withRoots,
  type Scenario,
} from './fixture.js';

export function bindingScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'CR3 code reuse distinguishes swapped executable pages even when the set of physical pages is unchanged',
      async run() {
        await withRoots(
          compiler,
          'IA32e',
          async (cpu) => {
            at(cpu, SHARED_PC + 16n);
            equal(
              (await cpu.run({ budget: 4n })).retired,
              4n,
              'prime both pages then exchange the PTEs',
            );
            at(cpu, 0x1ffbn);
            const old = await cpu.run({ budget: 2n });
            equal(old.state.registers[1], 0x11111111n, 'old first page');
            equal(old.state.registers[2], 0x22222222n, 'old second page');
            await switchRoot(cpu, ROOT_B);
            await switchRoot(cpu, ROOT_A);
            at(cpu, 0x1ffbn);
            const swapped = await cpu.run({ budget: 2n });
            equal(swapped.reason, 'budget', 'both remapped instructions complete');
            equal(swapped.state.registers[1], 0x33333333n, 'new first page');
            equal(swapped.state.registers[2], 0x44444444n, 'new second page');
            equal(
              swapped.resources.compilations,
              1,
              'ordered virtual-to-physical bindings changed',
            );
            equal(swapped.resources.pagingRestarts, 0, 'rejected before stale native execution');
          },
          {
            prepare(ram) {
              ram.set(hex('b911111111'), CODE_A + 0xffb);
              ram.set(hex('ba22222222f4'), NEXT_A);
              ram.set(hex('b933333333'), NEXT_A + 0xffb);
              ram.set(hex('ba44444444f4'), CODE_A);
              // Read [0x1000], [0x2000], then swap the two leaves through A's accessible PT alias.
              ram.set(
                hex('8b0425001000008b042500200000c704250840000063b00000c70425104000006390000082'),
                SHARED + 16,
              );
            },
          },
        );
      },
    },
    {
      name: 'CR3 code reuse reports missing physical execute backing only on reached fetch and resumes after repair',
      async run() {
        await withRoots(
          compiler,
          'IA32e',
          async (cpu) => {
            at(cpu, SHARED_PC + 16n);
            equal(
              (await cpu.run({ budget: 2n })).retired,
              2n,
              'prime A then redirect to unmapped backing',
            );
            at(cpu, PC + 7n);
            equal(
              (await cpu.run({ budget: 1n })).state.registers[1],
              0x110011aan,
              'old executable fill',
            );
            await switchRoot(cpu, ROOT_B);
            await switchRoot(cpu, ROOT_A);
            at(cpu, PC + 7n);
            const denied = await cpu.run({ budget: 1n });
            equal(
              denied.reason,
              'physical-memory-fault',
              'guest PTE present but private backing absent',
            );
            equal(denied.retired, 0n, 'instruction has not executed');
            equal(denied.physicalMemoryFault?.phase, 'final-access', 'payload execute');
            equal(denied.physicalMemoryFault?.physicalAddress, 0x18007n, 'unmapped physical byte');
            equal(denied.physicalMemoryFault?.cause, 'unmapped', 'missing backing');
            equal(cpu.readPagingState().cr2, 0xabcden, 'private refusal preserves CR2');
            const permissions = { read: true, write: true, execute: true };
            cpu.setMappings([
              { base: 0n, backing: 'ram', offset: 0, length: 0x14000, permissions },
              { base: 0x18000n, backing: 'ram', offset: CODE_B, length: 0x1000, permissions },
            ]);
            const repaired = await cpu.run({ budget: 1n });
            equal(repaired.reason, 'budget', 'repair allows original guest fetch');
            equal(repaired.state.registers[1], 0x220022bbn, 'new backing controls resumed code');
            equal(repaired.retired, 1n, 'exactly one resumed instruction');
          },
          {
            prepare(ram) {
              ram.set(hex('8b042500100000c70425084000006380010082'), SHARED + 16);
            },
          },
        );
      },
    },
  ];
}
