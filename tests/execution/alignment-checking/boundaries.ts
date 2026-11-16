import type { ExecutionContextSnapshot } from '../../../host/index.js';
import {
  AC,
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
  put,
  preserved,
  type Scenario,
} from './fixture.js';

export function boundaryScenarios(compiler: WebAssembly.Module): Scenario[] {
  const side = { createCpu, compiler };
  return [
    {
      name: 'alignment checking odd implicit GDT lookup stays exempt while SMAP still denies its user page under AC',
      async run() {
        for (const smap of [false, true]) {
          const f = fixture(compiler, '8e1b'),
            context = f.options.context as ExecutionContextSnapshot;
          context.gdtr = { base: 0x7001n, limit: 0x3f };
          f.bytes.set(new Uint8Array([0x13, 0]), 0xa000);
          // Selector13 indexes offset10 from the odd GDTR base: descriptor7011.
          f.bytes.set(new Uint8Array([0xff, 0xff, 0, 0, 0, 0xf3, 0xcf, 0]), 0xb011);
          if (smap) f.paging.cr4 |= 0x200000n;
          await using(side, f, async (cpu) => {
            const before = state({ 3: 0x6000n }),
              original = cpu.readContext(),
              unchanged = preserved(cpu),
              result = await runOne(cpu, before);
            equal(result.alignmentCheck, undefined, 'implicit descriptor alignment is exempt');
            if (smap) {
              equal(
                result.reason,
                'page-fault',
                'implicit supervisor traffic cannot use ordinary AC override',
              );
              same(
                result.pageFault,
                {
                  vector: 14,
                  instructionPc: PC,
                  address: 0x7011n,
                  access: 'read',
                  errorCode: 1,
                  table: 'gdt',
                  systemPhase: 'descriptor-read',
                  selector: 0x13,
                },
                'literal implicit SMAP provenance',
              );
              scalar(result, before);
            } else {
              equal(result.reason, 'budget', 'actual odd metadata lookup succeeds');
              scalar(result, before, 1n, 2);
            }
            same(
              cpu.readContext(),
              original,
              'no cache candidate on failure and equivalent descriptor on success',
            );
            equal(
              cpu.readPagingState().cr2,
              smap ? 0x7011n : 0xdeadn,
              'only implicit PF changes CR2',
            );
            unchanged();
            memory(cpu, f.bytes, 'already-accessed descriptor and warm page words unchanged');
            clock(cpu, smap ? 100n : 101n);
          });
        }
      },
    },
    {
      name: 'alignment checking CPL and SMAP consume the same live AC with separate meanings',
      async run() {
        for (const family of families)
          for (const cpl of [0, 1, 2, 3])
            for (const ac of [false, true]) {
              const f = fixture(compiler, '8b03', family),
                context = f.options.context as ExecutionContextSnapshot;
              context.cpl = cpl;
              for (const [name, segment] of Object.entries(context.segments)) {
                segment.dpl = cpl;
                segment.selector = (name === 'cs' ? 8 : 16) | cpl;
              }
              f.paging.cr4 |= 0x200000n;
              await using(side, f, async (cpu) => {
                const before = state({}, 2n | (ac ? AC : 0n)),
                  unchanged = preserved(cpu);
                const result = await runOne(cpu, before);
                if (cpl === 3 && ac) {
                  checkFault(result, 0x6001n, 4, 'read', 4);
                  scalar(result, before);
                } else if (cpl < 3 && !ac) {
                  equal(result.reason, 'page-fault', 'SMAP denied supervisor');
                  same(
                    result.pageFault,
                    {
                      vector: 14,
                      instructionPc: PC,
                      address: 0x6001n,
                      access: 'read',
                      errorCode: 1,
                    },
                    'SMAP is not AC',
                  );
                  equal(result.alignmentCheck, undefined, 'SMAP no AC packet');
                  scalar(result, before);
                } else {
                  equal(result.reason, 'budget', 'SMAP AC override or user AC0');
                  equal(result.alignmentCheck, undefined, 'CPL0/1/2 never natural AC');
                  const value =
                    family === 'ia32e' || family === 'fixed' ? 0x50403020n : 0x1122334450403020n;
                  scalar(result, before, 1n, 2, { 0: value });
                }
                unchanged();
                memory(cpu, f.bytes, 'SMAP no payload');
              });
            }
      },
    },
    {
      name: 'alignment checking final segmented linear address reverses effective-offset residues',
      async run() {
        for (const family of families)
          for (const compensated of [false, true])
            for (const ss of [false, true]) {
              const long = family === 'ia32e' || family === 'fixed';
              const code = long ? (ss ? '658b03' : '648b03') : ss ? '8b4500' : '8b03';
              const f = fixture(compiler, code, family),
                context = f.options.context as ExecutionContextSnapshot;
              const segment = long ? (ss ? 'gs' : 'fs') : ss ? 'ss' : 'ds';
              context.segments[segment].base = compensated ? 3n : 1n;
              await using(side, f, async (cpu) => {
                const offset = compensated ? 0x6001n : 0x6000n;
                const before = state({ 3: offset, 5: offset });
                const result = await runOne(cpu, before);
                if (compensated) {
                  equal(result.reason, 'budget', 'misaligned offset becomes aligned linear');
                  equal(result.alignmentCheck, undefined, 'no offset-only AC');
                  scalar(result, before, 1n, code.length / 2, {
                    0: long ? 0x80706050n : 0x1122334480706050n,
                  });
                } else {
                  checkFault(result, 0x6001n, 4, 'read', 4, segment, 0x6000n);
                  scalar(result, before);
                }
                memory(cpu, f.bytes, 'segment discriminator no writes');
              });
            }
      },
    },
    {
      name: 'alignment checking split-page admission precedes AC with literal A receipts and no D',
      async run() {
        for (const family of families)
          for (const write of [false, true])
            for (const missing of [false, true]) {
              const f = fixture(compiler, write ? '8903' : '8b03', family);
              f.leaf(0x6000, 0xa007n);
              f.leaf(0x7000, missing ? 0n : 0xb007n);
              await using(side, f, async (cpu) => {
                const before = state({ 3: 0x6fffn }),
                  unchanged = preserved(cpu),
                  wanted = f.bytes.slice();
                const result = await runOne(cpu, before);
                put(wanted, f.pt + 6 * f.width, 0xa027n, f.width);
                if (missing) {
                  equal(result.reason, 'page-fault', 'later missing page wins');
                  same(
                    result.pageFault,
                    {
                      vector: 14,
                      instructionPc: PC,
                      address: 0x7000n,
                      access: write ? 'write' : 'read',
                      errorCode: write ? 6 : 4,
                    },
                    'literal cross-page PF',
                  );
                  equal(result.alignmentCheck, undefined, 'PF excludes AC');
                } else {
                  put(wanted, f.pt + 7 * f.width, 0xb027n, f.width);
                  checkFault(result, 0x6fffn, 4, write ? 'write' : 'read', 4);
                }
                scalar(result, before);
                clock(cpu);
                unchanged();
                equal(
                  cpu.readPagingState().cr2,
                  missing ? 0x7000n : 0xdeadn,
                  'CR2 belongs only to PF',
                );
                memory(cpu, wanted, 'exact A-only full-RAM residue');
              });
            }
      },
    },
    {
      name: 'alignment checking retains fragment private provenance before AC and refuses no dirty metadata',
      async run() {
        for (const family of families)
          for (const dirty of [false, true]) {
            const f = fixture(compiler, '8903', family),
              slot = f.pt + 6 * f.width;
            f.leaf(0x6000, dirty ? 0xa027n : 0xa007n);
            f.leaf(0x7000, 0xb007n);
            const base = dirty ? slot : 0xb000,
              length = dirty ? f.width : 3;
            const permissions = { read: true, write: true, execute: true };
            f.options.memory!.mappings = [
              { base: 0n, backing: 'ram', offset: 0, length: base, permissions },
              {
                base: BigInt(base),
                backing: 'ram',
                offset: base,
                length,
                permissions: { ...permissions, write: false },
              },
              {
                base: BigInt(base + length),
                backing: 'ram',
                offset: base + length,
                length: f.bytes.length - base - length,
                permissions,
              },
            ];
            await using(side, f, async (cpu) => {
              const before = state({ 3: dirty ? 0x6001n : 0x6fffn }),
                wanted = f.bytes.slice();
              const result = await runOne(cpu, before);
              if (dirty) checkFault(result, 0x6001n, 4, 'write', 4);
              else {
                equal(result.reason, 'physical-memory-fault', 'private admission before AC');
                equal(result.alignmentCheck, undefined, 'private failure is not architectural');
                same(
                  result.physicalMemoryFault,
                  {
                    instructionPc: PC,
                    address: 0x7000n,
                    access: 'write',
                    physicalAddress: 0xb000n,
                    width: 3,
                    failingPhysicalAddress: 0xb000n,
                    physicalAccess: 'write',
                    cause: 'permission',
                    phase: 'final-access',
                    segment: 'ds',
                  },
                  'literal second-fragment private packet',
                );
                put(wanted, f.pt + 6 * f.width, 0xa027n, f.width);
                put(wanted, f.pt + 7 * f.width, 0xb027n, f.width);
              }
              scalar(result, before);
              equal(cpu.readPagingState().cr2, 0xdeadn, 'private/AC retain CR2');
              memory(cpu, wanted, 'no D publication or payload');
            });
          }
      },
    },
  ];
}
