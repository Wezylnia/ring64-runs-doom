import {
  AC,
  AUX,
  PC,
  createCpu,
  fixture,
  families,
  using,
  state,
  runOne,
  scalar,
  checkFault,
  clock,
  memory,
  same,
  equal,
  hex,
  put,
  type Scenario,
} from './fixture.js';

export function lifecycleScenarios(compiler: WebAssembly.Module): Scenario[] {
  const side = { createCpu, compiler };
  return [
    {
      name: 'alignment checking A-only source alias invalidates a faulted store and retry captures bytes before aliased D changes them again',
      async run() {
        for (const family of ['non-pae', 'fixed'] as const) {
          // MOV DWORD [BX/EBX/RBX],0x0000a007. Its immediate plus zero padding
          // is also the physical leaf word; A changes07→27 and D then27→67.
          const f = fixture(compiler, 'c70307a0000000000000', family),
            slot = f.pt + 6 * f.width;
          f.leaf(0x6000, 0xa007n);
          const permissions = { read: true, write: true, execute: true };
          f.options.memory!.mappings = [
            { base: 0n, backing: 'ram', offset: 0, length: slot, permissions },
            { base: BigInt(slot), backing: 'ram', offset: 0x8002, length: f.width, permissions },
            {
              base: BigInt(slot + f.width),
              backing: 'ram',
              offset: slot + f.width,
              length: f.bytes.length - slot - f.width,
              permissions,
            },
          ];
          await using(side, f, async (cpu) => {
            const before = state(),
              wanted = f.bytes.slice(),
              failed = await runOne(cpu, before);
            checkFault(failed, 0x6001n, 4, 'write', 4);
            scalar(failed, before);
            put(wanted, slot, 0xa027n, f.width);
            wanted[0x8002] = 0x27;
            memory(cpu, wanted, 'only reached A changed source and leaf alias, no data/D');
            const repaired = state({}, 2n),
              result = await runOne(cpu, repaired);
            equal(result.reason, 'budget', 'retry decodes the A-modified immediate');
            scalar(result, repaired, 1n, 6);
            put(wanted, slot, 0xa067n, f.width);
            wanted[0x8002] = 0x67;
            wanted.set(hex('27a00000'), 0xa001);
            memory(cpu, wanted, 'captured27 payload despite later D alias changing source to67');
            clock(cpu, 101n);
          });
        }
      },
    },
    {
      name: 'alignment checking warm middle entries observe live AM and AC without an AM-only source fence',
      async run() {
        for (const family of families) {
          const f = fixture(compiler, '908b0390', family);
          f.paging.cr0 &= ~AC;
          await using(side, f, async (cpu) => {
            const before = state();
            cpu.setState(before);
            await cpu.run({ budget: 2n });
            equal(cpu.inspectTranslations().length > 0, true, 'warm executable owner exists');
            for (const [am, ac] of [
              [true, true],
              [true, false],
              [false, true],
              [true, true],
              [false, false],
            ] as const) {
              const bank = cpu.readPagingState(),
                owners = cpu.inspectTranslations();
              cpu.setPagingState({ ...bank, cr0: am ? bank.cr0 | AC : bank.cr0 & ~AC });
              same(cpu.inspectTranslations(), owners, 'AM-only import retains exact source owners');
              if (bank.pdpte)
                same(cpu.readPagingState().pdpte, bank.pdpte, 'AM does not reload PDPTE bank');
              const input = { ...before, flags: 2n | (ac ? AC : 0n), rip: PC + 1n };
              const result = await runOne(cpu, input);
              if (am && ac) {
                equal(result.reason, 'alignment-check', 'middle entry sees active gate');
                same(
                  result.alignmentCheck,
                  {
                    instructionPc: PC + 1n,
                    vector: 17,
                    errorCode: 0,
                    segment: 'ds',
                    offset: 0x6001n,
                    address: 0x6001n,
                    width: 4,
                    access: 'read',
                    alignment: 4,
                  },
                  'middle-entry literal packet',
                );
                scalar(result, input);
              } else {
                equal(result.reason, 'budget', 'middle entry sees disabled gate');
                equal(result.alignmentCheck, undefined, 'no stale report on later success');
                scalar(result, input, 1n, 2, {
                  0: family === 'ia32e' || family === 'fixed' ? 0x50403020n : 0x1122334450403020n,
                });
              }
              memory(cpu, f.bytes, 'live policy changes no payload');
            }
          });
        }
      },
    },
    {
      name: 'alignment checking failed captured stores cannot publish during a later NOP or reset',
      async run() {
        for (const family of families) {
          const f = fixture(compiler, '890390', family);
          f.leaf(0x6000, 0xa027n);
          await using(side, f, async (cpu) => {
            const before = state(),
              failed = await runOne(cpu, before);
            checkFault(failed, 0x6001n, 4, 'write', 4);
            memory(cpu, f.bytes, 'no pending store payload or D');
            const input = { ...before, rip: PC + 2n },
              next = await runOne(cpu, input);
            equal(next.reason, 'budget', 'unrelated instruction succeeds');
            equal(next.alignmentCheck, undefined, 'old refusal does not leak');
            scalar(next, input, 1n, 1);
            memory(cpu, f.bytes, 'NOP cannot publish the rejected capture');
            if (family !== 'fixed') {
              cpu.reset();
              equal(cpu.readPagingState().cr0 & AC, 0n, 'physical reset clears AM');
              same(cpu.readState().event, { kind: 'ready' }, 'reset clears pending refusal state');
              memory(cpu, f.bytes, 'reset preserves image without deferred publication');
            }
          });
        }
      },
    },
    {
      name: 'alignment checking completed ENTER source alias invalidates old execution even when the later chain faults',
      async run() {
        for (const family of ['non-pae', 'fixed'] as const) {
          const f = fixture(compiler, 'c8000002', family),
            permissions = { read: true, write: true, execute: true };
          const width = family === 'fixed' ? 8 : 4,
            stackPhysical = family === 'fixed' ? 0x9ff8 : 0x9ffc;
          f.options.memory!.mappings = [
            { base: 0n, backing: 'ram', offset: 0, length: stackPhysical, permissions },
            {
              base: BigInt(stackPhysical),
              backing: 'ram',
              offset: 0x8000,
              length: width,
              permissions,
            },
            {
              base: 0xa000n,
              backing: 'ram',
              offset: 0xa000,
              length: f.bytes.length - 0xa000,
              permissions,
            },
          ];
          await using(side, f, async (cpu) => {
            const before = state({ 4: 0x5000n, 5: 0x6001n }),
              wanted = f.bytes.slice();
            const changed = hex(width === 8 ? '0160000000000000' : '01600000');
            wanted.set(changed, 0x8000);
            wanted.set(changed, stackPhysical);
            const failed = await runOne(cpu, before);
            checkFault(failed, width === 8 ? 0x5ff9n : 0x5ffdn, width, 'read', width, 'ss');
            scalar(failed, before);
            memory(cpu, wanted, 'one prior push mutates both source aliases');
            const retry = state({ 0: 0x6000n, 4: 0x5000n, 5: 0x6001n }),
              done = await runOne(cpu, retry);
            equal(done.reason, 'budget', 'changed source decoded on retry');
            // 016000 is ADD DWORD [EAX+0],ESP; a stale ENTER would fault again.
            scalar(done, retry, 1n, 3);
            wanted.set(hex('10703040'), 0xa000);
            memory(cpu, wanted, 'fresh ADD, no duplicated ENTER push');
            clock(cpu, 101n);
          });
        }
      },
    },
    {
      name: 'alignment checking fault suppresses completion TF data watchpoints while repaired completion fires once',
      async run() {
        const f = fixture(compiler, '8b03');
        await using(side, f, async (cpu) => {
          const before = state({}, AC | 0x102n);
          cpu.setDebugState({ addresses: [0x6000n, 0n, 0n, 0n], dr6: 0n, dr7: 0xf0401n });
          const debug = cpu.readDebugState(),
            failed = await runOne(cpu, before);
          checkFault(failed, 0x6001n, 4, 'read', 4);
          scalar(failed, before);
          same(cpu.readDebugState(), debug, 'no B0 or BS from failed access');
          const repaired = state({ 3: 0x6000n }, AC | 0x102n),
            result = await runOne(cpu, repaired);
          equal(result.reason, 'debug-exception', 'completed data access and TF share trap');
          equal(result.retired, 1n, 'repaired source retires once');
          same(
            result.debugException,
            {
              vector: 1,
              class: 'trap',
              cause: 'combined',
              breakpoints: 1,
              singleStep: true,
              instructionPc: PC,
              resumePc: PC + 2n,
              instructionCompleted: true,
              resumeFlags: AC | 0x102n,
            },
            'exact combined completed boundary',
          );
          same(
            cpu.readDebugState(),
            { ...debug, dr6: debug.dr6 | 0x4001n },
            'one B0+BS completion',
          );
          equal(result.alignmentCheck, undefined, 'no stale AC with completion DB');
          clock(cpu, 101n);
        });
      },
    },
    {
      name: 'alignment checking execute DB precedes operand and RF suppression still permits an operand fault',
      async run() {
        const f = fixture(compiler, '8b03');
        await using(side, f, async (cpu) => {
          cpu.setDebugState({ addresses: [PC, 0n, 0n, 0n], dr6: 0n, dr7: 0x401n });
          const before = state(),
            first = await runOne(cpu, before);
          equal(first.reason, 'debug-exception', 'pre-instruction execute breakpoint');
          equal(first.retired, 0n, 'execute DB no retirement');
          equal(first.alignmentCheck, undefined, 'operand not reached');
          const resumed = state({}, AC | 0x10002n),
            second = await runOne(cpu, resumed);
          checkFault(second, 0x6001n, 4, 'read', 4);
          scalar(second, resumed, 0n, 0, {}, AC | 2n);
          memory(cpu, f.bytes, 'DB and AC publish no operand payload');
        });
      },
    },
    {
      name: 'alignment checking failed instructions preserve MAX TSC while the repaired retirement wraps once',
      async run() {
        const f = fixture(compiler, '8b03');
        await using(side, f, async (cpu) => {
          const before = state();
          cpu.setState(before);
          cpu.setTimeState({ tsc: 0xffffffffffffffffn, tscAux: AUX });
          const first = await cpu.run({ budget: 1n });
          checkFault(first, 0x6001n, 4, 'read', 4);
          clock(cpu, 0xffffffffffffffffn);
          cpu.setState(state({ 3: 0x6000n }));
          const second = await cpu.run({ budget: 1n });
          equal(second.retired, 1n, 'one repaired retirement');
          clock(cpu, 0n);
        });
      },
    },
  ];
}
