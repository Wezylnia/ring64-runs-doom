import { createCpu } from '../../../host/index.js';
import { initial } from '../../support/fixture.js';
import { FAST_FORMS, USER, USER_SP, fastSetup } from '../context-reuse/warm.js';
import {
  at,
  equal,
  hex,
  little,
  PC,
  ROOT_A,
  ROOT_B,
  SHARED,
  SHARED_PC,
  switchRoot,
  withRoots,
  word,
  type Scenario,
} from './fixture.js';

export function controlScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'CR3 code reuse rechecks an active global 4K source after a nonglobal 2M fill shadows it at the same physical address',
      async run() {
        await withRoots(
          compiler,
          'IA32e',
          async (cpu) => {
            const pc = 0x9000n;
            at(cpu, pc);
            equal(
              (await cpu.run({ budget: 1n })).state.registers[1],
              0x66554433n,
              'global 4K A fetch',
            );
            await switchRoot(cpu, ROOT_B | 2n);
            at(cpu, pc);
            const retained = await cpu.run({ budget: 1n });
            equal(retained.state.registers[1], 0x66554433n, 'B fetched through A global entry');
            equal(retained.resources.compilations, 1, 'B owner subscribes to original 4K walk');
            at(cpu, pc);
            equal(
              (await cpu.run({ budget: 1n })).resources.cacheHits,
              1,
              'B owner initially eligible',
            );

            at(cpu, 0xa000n);
            const neighbor = await cpu.run({ budget: 1n });
            equal(neighbor.state.rip, 0xa001n, 'neighbor fetch fills nonglobal 2M B entry');
            at(cpu, pc);
            const rebound = await cpu.run({ budget: 1n });
            equal(rebound.state.registers[1], 0x66554433n, 'physical instruction still identical');
            equal(
              rebound.resources.compilations,
              1,
              'new walk cannot use old global source subscriptions',
            );
            equal(rebound.resources.pagingRestarts, 0, 'changed walk detected before entry');
          },
          {
            pge: true,
            pcide: true,
            prepare(ram) {
              ram.set(little(0x9163n), 0x4000 + 9 * 8); // A VA 0x9000 via global 4K PTE
              ram.set(little(0xe3n), 0x7000); // B VA [0,2M) via non-global identity PDE
              ram.set(hex('b933445566f4'), 0x9000);
              ram.set(hex('90f4'), 0xa000);
            },
          },
        );
      },
    },
    {
      name: 'CR3 code reuse refuses the old walk when a new leaf table reaches the same physical code',
      async run() {
        await withRoots(
          compiler,
          'IA32e',
          async (cpu) => {
            at(cpu, SHARED_PC + 16n);
            equal((await cpu.run({ budget: 2n })).retired, 2n, 'prime old walk and reroute PDE');
            equal(word(cpu, 0x3000n), 0xd063n, 'A PDE now points to cloned PT');
            at(cpu, PC + 7n);
            const original = await cpu.run({ budget: 1n });
            equal(original.state.registers[1], 0x110011aan, 'old walk admits A code');
            equal(original.resources.compilations, 1, 'registered after reroute');
            await switchRoot(cpu, ROOT_B);
            await switchRoot(cpu, ROOT_A);
            at(cpu, PC + 7n);
            const current = await cpu.run({ budget: 1n });
            equal(current.state.registers[1], 0x110011aan, 'new walk reaches identical code');
            equal(current.resources.compilations, 1, 'original walk subscription is insufficient');
            cpu.writePhysicalMemory(0xd008n, little(0xa063n));
            at(cpu, PC + 7n);
            equal(
              (await cpu.run({ budget: 1n })).state.registers[1],
              0x220022bbn,
              'new PT edit observed',
            );
          },
          {
            prepare(ram) {
              ram.set(little(0x3063n), 0x4028); // A linear 0x5000 aliases its PDE
              ram.copyWithin(0xd000, 0x4000, 0x5000);
              ram.set(hex('8b042500100000c704250050000063d0000082'), SHARED + 16);
            },
          },
        );
      },
    },
    {
      name: 'CR3 code reuse follows retained PCID and global fetch translations until a real invalidation',
      async run() {
        for (const global of [false, true]) {
          await withRoots(
            compiler,
            'IA32e',
            async (cpu) => {
              const a = ROOT_A | 1n,
                b = ROOT_B | 1n;
              await switchRoot(cpu, b);
              at(cpu, PC + 7n);
              equal(
                (await cpu.run({ budget: 1n })).state.registers[1],
                0x220022bbn,
                'first B bytes',
              );
              if (global) {
                at(cpu, SHARED_PC + 16n);
                equal((await cpu.run({ budget: 1n })).retired, 1n, 'remove B global code before A');
              }
              await switchRoot(cpu, a);
              at(cpu, PC + 7n);
              equal(
                (await cpu.run({ budget: 1n })).state.registers[1],
                0x110011aan,
                'prime A fetch',
              );
              await switchRoot(cpu, global ? b : b | (1n << 63n));
              at(cpu, PC + 7n);
              equal(
                (await cpu.run({ budget: 1n })).state.registers[1],
                0x110011aan,
                'retained A fetch wins under B',
              );
              if (global) {
                at(cpu, SHARED_PC + 16n);
                equal((await cpu.run({ budget: 1n })).retired, 1n, 'remove retained global');
              } else await switchRoot(cpu, b);
              at(cpu, PC + 7n);
              equal(
                (await cpu.run({ budget: 1n })).state.registers[1],
                0x220022bbn,
                'refilled B fetch',
              );
            },
            {
              pcide: true,
              pge: global,
              prepare(ram) {
                if (global)
                  for (const [slot, page] of [
                    [0x4008, 0x9163n],
                    [0x8008, 0xa163n],
                  ] as const)
                    ram.set(little(page), slot);
                ram.set(hex('0f013c250010000082'), SHARED + 16); // INVLPG [0x1000]
              },
            },
          );
        }
      },
    },
    {
      name: 'CR3 code reuse distinguishes a reloaded legacy PAE quartet on return to the same PDPT address',
      async run() {
        await withRoots(compiler, 'PAE', async (cpu) => {
          at(cpu, PC + 7n);
          equal(
            (await cpu.run({ budget: 1n })).state.registers[1],
            0x110011aan,
            'original A quartet',
          );
          await switchRoot(cpu, ROOT_B);
          cpu.writePhysicalMemory(ROOT_A, little(0x6001n));
          await switchRoot(cpu, ROOT_A);
          equal(cpu.readPagingState().pdpte?.[0], 0x6001n, 'A loaded alternate directory');
          at(cpu, PC + 7n);
          const alternate = await cpu.run({ budget: 1n });
          equal(alternate.state.registers[1], 0x220022bbn, 'installed directory selects B code');
          equal(alternate.resources.compilations, 1, 'A address alone cannot admit old owner');
        });
      },
    },
    {
      name: 'CR3 code reuse retains user and kernel contexts through SYSCALL, two root writes and SYSRET',
      async run() {
        const setup = fastSetup(compiler, FAST_FORMS[0]);
        setup.ram.set(little(0x2027n), 0x6000); // alternate root shares user/kernel pages
        setup.ram.set(hex('b8006000000f22d8b8001000000f22d8480f07'), 0xa000);
        const cpu = await createCpu(setup.options);
        try {
          cpu.setState(initial(USER, { 4: USER_SP, 8: 8n }, 0x202n));
          for (let i = 0; i < 16; i++) {
            const one = await cpu.run({ budget: 1n });
            equal(one.retired, 1n, 'prime two syscall round trips under quota one');
          }
          equal(cpu.readState().rip, USER, 'back at user loop');
          const hot = await cpu.run({ budget: 48n });
          equal(hot.reason, 'budget', 'six remaining round trips');
          equal(hot.retired, 48n, 'no duplicate retirement');
          equal(hot.state.rip, USER + 7n, 'fallthrough after final user branch');
          equal(hot.state.registers[8], 0n, 'guest trip counter exhausted');
          equal(cpu.readContext().cpl, 3, 'user privileges restored');
          equal(cpu.readPagingState().cr3, ROOT_A, 'user CR3 restored');
          equal(hot.resources.compilations, 0, 'no new code within resident user/kernel loops');
          equal(hot.resources.generatedBytes, 0, 'no new translation bytes');
        } finally {
          cpu.dispose();
        }
      },
    },
  ];
}
