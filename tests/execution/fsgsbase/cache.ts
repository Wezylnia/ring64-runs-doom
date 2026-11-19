import { type Cpu, createCpu } from '../../../sdk/src/index.js';
import { Core } from '../../../sdk/src/wasm/core.js';
import { check, equal, hex, initial, LIMITS, type Scenario } from '../../support/fixture.js';
import { fixture, put, using } from '../supervisor-access/fixture.js';

const pc = 0x1000n,
  a = 0x6000n,
  b = 0x7000n;
const first = 0x1111222233334444n,
  second = 0xaaaabbbbccccddddn;
function setup(compiler: WebAssembly.Module, segment: 'fs' | 'gs', capacity = 16) {
  const isFs = segment === 'fs',
    prefix = isFs ? '64' : '65';
  // WR{FS,GS}BASE R10; MOV RAX,{FS,GS}:[RBX]; JMP $. At +20h, a separate translatable consumer.
  const f = fixture(
    { compiler, createCpu },
    'ia32e',
    (isFs ? 'f3490faed2' : 'f3490faeda') + prefix + '488b03ebfe',
    { cpl: 3 },
  );
  f.paging.cr4 |= 0x10000n;
  f.leaf(Number(a), 0xa067n);
  f.leaf(Number(b), 0xb067n);
  put(f.bytes, 0xa000, first);
  put(f.bytes, 0xb000, second);
  f.bytes.set(hex(prefix + '488b03ebfe'), 0x8020);
  f.options.limits = { ...LIMITS, maxCachedArtifacts: capacity };
  return f;
}
async function at(cpu: Cpu, rip: bigint, target: bigint, budget = 1n) {
  cpu.setState(initial(rip, { 3: 0n, 10: target }));
  return cpu.run({ budget });
}
export function cacheScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'guest CR4.FSGSBASE toggles preserve a stale data translation until INVLPG',
      async run() {
        // MOV R10,[RBX]; MOV [RDI],RAX; MOV CR4,RAX; RDGSBASE RCX; INVLPG [RBX].
        for (const family of ['ia32e', 'fixed'] as const) {
          const side = { compiler, createCpu };
          const f = fixture(side, family, '4c8b134889070f22e0f3480faec90f013b');
          f.leaf(0x5000, BigInt(f.pt) | 0x63n); // supervisor guest alias of the leaf table
          f.leaf(Number(a), 0xa063n); // A=1, D=1, non-global.
          put(f.bytes, 0xa000, first);
          put(f.bytes, 0xb000, second);
          await using(side, f, async (cpu) => {
            const run = async (offset: number, registers: Record<number, bigint> = {}) => {
              cpu.setState(initial(pc + BigInt(offset), { 3: a, ...registers }));
              return cpu.run({ budget: 1n });
            };
            equal((await run(0)).state.registers[10], first, 'original data translation');
            equal(
              (await run(3, { 0: 0xb063n, 7: 0x5030n })).reason,
              'budget',
              'guest retargets data PTE',
            );
            equal(
              (await run(0)).state.registers[10],
              first,
              'original mapping is resident after guest PTE edit',
            );
            const initialCr4 = cpu.readPagingState().cr4;
            for (const enabled of [true, false]) {
              const cr4 = enabled ? initialCr4 | 0x10000n : initialCr4;
              equal((await run(6, { 0: cr4 })).reason, 'budget', 'guest MOV CR4');
              equal(cpu.readPagingState().cr4, cr4, 'actual gate toggled');
              equal(
                (await run(9)).reason,
                enabled ? 'budget' : 'invalid-encoding',
                'live RDGSBASE gate',
              );
              equal(
                (await run(0)).state.registers[10],
                first,
                'changing the gate retains the stale data translation',
              );
            }
            equal((await run(14)).reason, 'budget', 'guest INVLPG');
            equal(
              (await run(0)).state.registers[10],
              second,
              'fresh walk observes retargeted backing',
            );
          });
        }
      },
    },
    {
      name: 'FSGSBASE changing and identical writes lead to the correct scalar consumer with retained or evicted owners',
      async run() {
        for (const segment of ['fs', 'gs'] as const)
          for (const capacity of [1, 16]) {
            await using(
              { compiler, createCpu },
              setup(compiler, segment, capacity),
              async (cpu) => {
                const ctx = cpu.readContext();
                ctx.segments[segment].base = a;
                cpu.setContext(ctx);
                for (let pass = 0; pass < 2; pass++)
                  for (const base of [a, b, b, a]) {
                    const r = await at(cpu, pc, base, 2n);
                    equal(r.reason, 'budget', 'write followed by load');
                    equal(r.retired, 2n, 'each instruction retires once');
                    equal(
                      r.state.registers[0],
                      base === a ? first : second,
                      'scalar load uses current hidden base',
                    );
                    equal(cpu.readContext().segments[segment].base, base, 'architectural base');
                    if (pass === 1 && capacity === 16)
                      equal(
                        r.resources.compilations,
                        0,
                        'returned-to and unchanged contexts reuse their owners',
                      );
                    check(
                      cpu.inspectTranslations().length <= capacity,
                      'bounded installed artifacts',
                    );
                  }
                const high = 0xffff800000000000n,
                  fault = await at(cpu, pc, high, 2n);
                equal(
                  fault.reason,
                  'page-fault',
                  'only the following load dereferences the high canonical base',
                );
                equal(fault.state.rip, pc + 5n, 'fault belongs to MOV');
                equal(fault.retired, 1n, 'base write already retired');
                equal(fault.pageFault?.address, high, 'new high base');
                equal(fault.pageFault?.errorCode, 4, 'nonpresent user read');
                equal(
                  cpu.readContext().segments[segment].base,
                  high,
                  'base retained on consumer failure',
                );
                equal(
                  (await at(cpu, pc, a, 2n)).state.registers[0],
                  first,
                  'original context can be reached after fault',
                );
              },
            );
          }
      },
    },
    {
      name: 'an unpublished TLS consumer survives an identical guest write and cannot install after a guest base ABA',
      async run() {
        const create = Core.create;
        let core: Core | undefined;
        Core.create = async (...args) => {
          const instance = await create(...args);
          core = instance;
          return instance;
        };
        let cpu: Cpu;
        try {
          cpu = await createCpu(setup(compiler, 'gs').options);
        } finally {
          Core.create = create;
        }
        try {
          check(core, 'captured real Core from public CPU');
          const ctx = cpu.readContext();
          ctx.segments.gs.base = a;
          cpu.setContext(ctx);
          for (const change of [false, true]) {
            const unit = core.compile(pc + 0x20n, LIMITS);
            check(unit, 'deferred GS consumer available');
            const owner = unit.metadata.owner;
            try {
              // Hold browser installation until the public guest finishes both writes.
              const compiled = WebAssembly.compile(new Uint8Array(unit.bytes));
              equal((await at(cpu, pc, change ? b : a)).reason, 'budget', 'first real guest write');
              equal((await at(cpu, pc, a)).reason, 'budget', 'second real guest write');
              await compiled;
              equal(
                core.publish(owner),
                !change,
                'intervening base changes cancel unpublished owners even after returning to A',
              );
            } finally {
              core.release(owner);
            }
          }
          const fresh = await at(cpu, pc + 0x20n, a);
          equal(fresh.state.registers[0], first, 'fresh installation is usable');
        } finally {
          cpu.dispose();
        }
      },
    },
  ];
}
