import {
  at,
  CODE_A,
  equal,
  hex,
  little,
  NEXT_A,
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

export function sourceScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'CR3 code reuse observes dormant executable edits from a host physical write or guest virtual alias',
      async run() {
        for (const guest of [false, true]) {
          await withRoots(
            compiler,
            'IA32e',
            async (cpu) => {
              at(cpu, PC + 7n);
              equal(
                (await cpu.run({ budget: 1n })).state.registers[1],
                0x110011aan,
                'prime original A',
              );
              await switchRoot(cpu, ROOT_B);
              at(cpu, PC + 7n);
              equal((await cpu.run({ budget: 1n })).state.registers[1], 0x220022bbn, 'private B');
              if (guest) {
                at(cpu, SHARED_PC + 16n);
                equal((await cpu.run({ budget: 1n })).retired, 1n, 'guest alias store');
              } else {
                cpu.writePhysicalMemory(BigInt(CODE_A + 8), little(0x33443344n, 4));
              }
              await switchRoot(cpu, ROOT_A);
              at(cpu, PC + 7n);
              const fresh = await cpu.run({ budget: 1n });
              equal(fresh.state.registers[1], 0x33443344n, 'recompiled edited immediate');
              equal(fresh.resources.compilations, 1, 'revoked physical source');
            },
            {
              prepare(ram) {
                ram.set(hex('c70425083000004433443382'), SHARED + 16);
              },
            },
          );
        }
      },
    },
    {
      name: 'CR3 code reuse rejects source discovered through a stale instruction mapping before registration',
      async run() {
        await withRoots(
          compiler,
          'IA32e',
          async (cpu) => {
            at(cpu, SHARED_PC + 16n);
            const retarget = await cpu.run({ budget: 2n });
            equal(retarget.retired, 2n, 'fill old mapping and retarget A PTE');
            equal(word(cpu, 0x4008n), 0xa063n, 'page table now names B code');
            at(cpu, PC + 7n);
            const stale = await cpu.run({ budget: 1n });
            equal(stale.state.registers[1], 0x110011aan, 'old authoritative fill');
            equal(stale.resources.compilations, 1, 'new owner records the stale translation');
            await switchRoot(cpu, ROOT_B);
            await switchRoot(cpu, ROOT_A);
            at(cpu, PC + 7n);
            const fresh = await cpu.run({ budget: 1n });
            equal(fresh.state.registers[1], 0x220022bbn, 'effective A mapping now reaches B bytes');
            equal(fresh.resources.compilations, 1, 'cold admission refused obsolete A binding');
            equal(fresh.resources.pagingRestarts, 0, 'obsolete owner never ran');
          },
          {
            prepare(ram) {
              // Read [0x1000] as data, then write the new low PTE through its guest alias.
              ram.set(hex('8b042500100000c704250840000063a0000082'), SHARED + 16);
            },
          },
        );
      },
    },
    {
      name: 'CR3 code reuse keeps NX and accessed effects of a speculative second-page suffix behind the actual branch',
      async run() {
        await withRoots(
          compiler,
          'IA32e',
          async (cpu) => {
            at(cpu, SHARED_PC + 16n);
            equal(
              (await cpu.run({ budget: 3n })).retired,
              3n,
              'prime suffix then set NX and clear A',
            );
            const nxPte = 0x8000_0000_0000_b003n;
            equal(word(cpu, 0x4010n), nxPte, 'old executable TLB fill precedes registration');
            at(cpu, PC);
            equal((await cpu.run({ budget: 2n })).state.registers[1], 7n, 'untaken crossing edge');
            await switchRoot(cpu, ROOT_B);
            await switchRoot(cpu, ROOT_A);
            at(cpu, PC);
            const stop = await cpu.run({ budget: 1n, stop: PC });
            equal(stop.reason, 'stop', 'stop wins before candidate admission');
            equal((await cpu.run({ budget: 0n })).retired, 0n, 'zero budget');
            const fallthrough = await cpu.run({ budget: 2n });
            equal(fallthrough.reason, 'budget', 'untaken NX branch has no architectural fault');
            equal(fallthrough.state.rip, PC + 11n, 'fallthrough MOV completed');
            equal(fallthrough.state.registers[1], 7n, 'fallthrough value');
            equal(
              fallthrough.resources.compilations,
              1,
              'invalid suffix disqualifies the old unit',
            );
            equal(cpu.readPagingState().cr2, 0xabcden, 'speculative candidate preserved CR2');
            equal(word(cpu, 0x4010n), nxPte, 'unreached suffix never set A');
            cpu.setState({ ...fallthrough.state, rip: PC, flags: fallthrough.state.flags | 0x40n });
            const reached = await cpu.run({ budget: 2n });
            equal(reached.reason, 'page-fault', 'crossing MOV now fetches the NX suffix');
            equal(reached.retired, 1n, 'only taken JZ retired');
            equal(reached.state.rip, 0x1ffen, 'crossing instruction PC');
            equal(reached.pageFault?.address, 0x2000n, 'first blocked suffix byte');
            equal(reached.pageFault?.errorCode, 17, 'present supervisor instruction fetch');
            equal(cpu.readPagingState().cr2, 0x2000n, 'required fault publishes CR2');
          },
          {
            prepare(ram) {
              ram.set(hex('0f84f80f0000b907000000f4'), CODE_A); // JZ 0x1ffe; MOV ECX,7; HLT
              ram.set(hex('b811'), CODE_A + 0xffe);
              ram.set(hex('223344f4'), NEXT_A);
              // Read suffix as data; clear low A and set NX in the high half of its PTE.
              ram.set(
                hex('8b042500200000c704251040000003b00000c70425144000000000008082'),
                SHARED + 16,
              );
            },
          },
        );
      },
    },
    {
      name: 'CR3 code reuse preserves stop, budget and execution breakpoint before a missing source at quota one',
      async run() {
        for (const stopFirst of [false, true]) {
          await withRoots(
            compiler,
            'IA32e',
            async (cpu) => {
              const debug = cpu.readDebugState();
              const target = PC + 20n;
              cpu.setDebugState({ ...debug, addresses: [0xdeadn, 0n, 0n, 0n], dr7: 0x401n });
              at(cpu, SHARED_PC + 16n);
              equal((await cpu.run({ budget: 2n })).retired, 2n, 'prime A fill then remove PTE');
              at(cpu, PC);
              equal(
                (await cpu.run({ budget: 1n })).retired,
                1n,
                'discover A body through stale fill',
              );
              await switchRoot(cpu, ROOT_B);
              cpu.setDebugState({ ...debug, addresses: [target, 0n, 0n, 0n], dr7: 0x401n });
              at(cpu, PC + 17n, ROOT_A); // cold B MOV CR3 consumes the only translation
              const switched = await cpu.run({
                budget: 2n,
                ...(stopFirst ? { stop: target } : {}),
              });
              equal(
                switched.reason,
                stopFirst ? 'stop' : 'debug-exception',
                'boundary on returned A',
              );
              equal(switched.retired, 1n, 'only B root writer completed');
              equal(switched.resources.compilations, 1, 'B exhausted translation allowance');
              equal(switched.state.rip, target, 'A successor even though its PTE is absent');
              if (stopFirst) {
                equal(
                  (await cpu.run({ budget: 0n })).reason,
                  'budget',
                  'zero budget outranks breakpoint',
                );
                const trap = await cpu.run({ budget: 1n });
                equal(trap.reason, 'debug-exception', 'breakpoint precedes required fetch');
                equal(trap.retired, 0n, 'no A instruction');
                equal(trap.resources.compilations, 0, 'no compilation to select breakpoint');
              }
              equal(cpu.readPagingState().cr2, 0xabcden, 'cold proof and debug left CR2 alone');
              equal(cpu.readDebugState().dr6 & 1n, 1n, 'matching execution slot selected');
              cpu.setDebugState(debug);
              const fault = await cpu.run({ budget: 1n });
              equal(fault.reason, 'page-fault', 'actual A fetch needs the missing page');
              equal(fault.retired, 0n, 'root writer not replayed');
              equal(fault.pageFault?.address, target, 'precise missing first byte');
              equal(fault.pageFault?.errorCode, 16, 'supervisor nonpresent fetch');
              equal(cpu.readPagingState().cr2, target, 'required fetch updates CR2');
            },
            {
              translations: 1,
              prepare(ram) {
                ram.set(hex('8b042500100000c70425084000000000000082'), SHARED + 16);
              },
            },
          );
        }
      },
    },
  ];
}
