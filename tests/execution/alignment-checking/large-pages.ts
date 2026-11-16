import {
  PC,
  createCpu,
  fixture,
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
  type Family,
} from './fixture.js';

export function largePageScenarios(compiler: WebAssembly.Module): Scenario[] {
  const side = { createCpu, compiler };
  return [
    {
      name: 'alignment checking 4MiB 2MiB and 1GiB boundary requests keep exact selected admission priority',
      async run() {
        const cases: { family: Family; size: number }[] = [
          { family: 'non-pae', size: 0x400000 },
          { family: 'pae', size: 0x200000 },
          { family: 'ia32e', size: 0x200000 },
          { family: 'fixed', size: 0x200000 },
          { family: 'ia32e', size: 0x40000000 },
          { family: 'fixed', size: 0x40000000 },
        ];
        for (const { family, size } of cases)
          for (const write of [false, true])
            for (const missing of [false, true]) {
              const f = fixture(compiler, write ? '8903' : '8b03', family),
                width = family === 'non-pae' ? 4 : 8;
              f.paging.cr3 = 0x10000n;
              f.bytes.fill(0, 0x10000, 0x13000);
              f.bytes.set(hex(write ? '8903' : '8b03'), Number(PC));
              let leaf = 0x10000;
              if (family === 'non-pae') f.paging.cr4 |= 0x10n;
              else if (family === 'pae') {
                f.paging.pdpte = [0x11001n, 0n, 0n, 0n];
                put(f.bytes, 0x10000, 0x11001n, 8);
                leaf = 0x11000;
              } else {
                put(f.bytes, 0x10000, 0x11027n, 8);
                leaf = size === 0x40000000 ? 0x11000 : 0x12000;
                if (size !== 0x40000000) put(f.bytes, 0x11000, 0x12027n, 8);
              }
              // Fetch shares the first large leaf: it is already A1,D0. The data
              // access must reach/update only the second leaf before #AC or #PF.
              put(f.bytes, leaf, 0xa7n, width);
              put(f.bytes, leaf + width, missing ? 0n : BigInt(size) | 0x87n, width);
              f.options.memory!.mappings.push(
                {
                  base: BigInt(size - 1),
                  backing: 'ram',
                  offset: 0x20000,
                  length: 1,
                  permissions: { read: true, write: true, execute: false },
                },
                {
                  base: BigInt(size),
                  backing: 'ram',
                  offset: 0x21000,
                  length: 3,
                  permissions: { read: true, write: true, execute: false },
                },
              );
              await using(side, f, async (cpu) => {
                const before = state({ 3: BigInt(size - 1) }),
                  wanted = f.bytes.slice(),
                  result = await runOne(cpu, before);
                if (missing) {
                  equal(result.reason, 'page-fault', 'large second-leaf PF before AC');
                  same(
                    result.pageFault,
                    {
                      vector: 14,
                      instructionPc: PC,
                      address: BigInt(size),
                      access: write ? 'write' : 'read',
                      errorCode: write ? 6 : 4,
                    },
                    'literal large-page boundary PF',
                  );
                  equal(result.alignmentCheck, undefined, 'large PF is not AC');
                } else {
                  checkFault(result, BigInt(size - 1), 4, write ? 'write' : 'read', 4);
                  put(wanted, leaf + width, BigInt(size) | 0xa7n, width);
                }
                scalar(result, before);
                equal(
                  cpu.readPagingState().cr2,
                  missing ? BigInt(size) : 0xdeadn,
                  'large-page CR2 provenance',
                );
                memory(cpu, wanted, 'literal large-leaf A-only mutation without D/payload');
                clock(cpu);
              });
            }
      },
    },
  ];
}
