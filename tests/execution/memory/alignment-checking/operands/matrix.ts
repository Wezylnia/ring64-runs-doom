import {
  using as usingModel,
  profiles,
  type Profile,
} from '../../../processor/identity-time/oracle.js';
import {
  createCpu,
  AC,
  payload,
  hex,
  state,
  scalar,
  checkFault,
  noOtherFault,
  clock,
  equal,
  same,
  bytes,
  preserved,
  type Scenario,
} from '../fixture.js';

// Authored independently of the paging validators and decoder under test.
export const cells: Profile[] = [
  ...profiles.map((p) => ({ ...p, family: 'linear' as const })),
  ...profiles
    .filter((p) => ['real', 'protected', 'virtual8086'].includes(p.mode))
    .map((p) => ({ ...p, family: 'pg0' as const })),
  ...(['legacy32', 'pae'] as const).flatMap((family) =>
    profiles
      .filter((p) => ['protected', 'virtual8086'].includes(p.mode))
      .map((p) => ({ ...p, family })),
  ),
  ...(['ia32e', 'fixed'] as const).flatMap((family) =>
    profiles
      .filter((p) => ['compatibility', 'long64'].includes(p.mode))
      .map((p) => ({ ...p, family })),
  ),
];

export function matrixScenarios(compiler: WebAssembly.Module): Scenario[] {
  return ['linear', 'pg0', 'legacy32', 'pae', 'ia32e', 'fixed'].map((family) => ({
    name: `alignment checking literal scalar matrix ${family} across forty-three admitted cells`,
    async run() {
      equal(cells.length, 43, 'all authored model geometry cells');
      for (const cell of cells.filter((p) => p.family === family))
        for (const width of [2, 4] as const)
          for (const write of [false, true]) {
            const profile = { ...cell, cpl: cell.mode === 'real' ? 0 : 3 };
            const code =
              (cell.bits === 16 ? (width === 4 ? '66' : '') : width === 2 ? '66' : '') +
              (write ? '89' : '8b') +
              (cell.bits === 16 ? '07' : '03');
            await usingModel({ createCpu, compiler }, code, profile, async (cpu) => {
              const putData = () =>
                family === 'linear'
                  ? cpu.writeMemory(0x6000n, hex(payload))
                  : cpu.writePhysicalMemory(0x6000n, hex(payload));
              const ram = () =>
                family === 'linear'
                  ? cpu.readMemory(0n, 0x50000)
                  : cpu.readPhysicalMemory(0n, 0x50000);
              for (const am of family === 'linear' ? [false] : [false, true]) {
                if (family !== 'linear') {
                  const bank = cpu.readPagingState();
                  cpu.setPagingState({ ...bank, cr0: am ? bank.cr0 | AC : bank.cr0 & ~AC });
                  equal(cpu.readPagingState().cr0 & AC, am ? AC : 0n, 'AM round trip');
                }
                for (const ac of [false, true])
                  for (const address of [0x6000n, 0x6001n]) {
                    putData();
                    const beforeRam = ram(),
                      wanted = beforeRam.slice();
                    const before = state(
                      { 3: address },
                      2n | (cell.mode === 'virtual8086' ? 0x3000n : 0n) | (ac ? AC : 0n),
                    );
                    cpu.setState(before);
                    cpu.setTimeState({ tsc: 100n, tscAux: 0x89abcdef });
                    const numeric = preserved(cpu),
                      controls = family === 'linear' ? undefined : cpu.readPagingState(),
                      context = cpu.readContext();
                    const result = await cpu.run({ budget: 1n, repeatBudget: 7n });
                    const fault = am && ac && cell.mode !== 'real' && address === 0x6001n;
                    if (fault) {
                      checkFault(result, address, width, write ? 'write' : 'read', width);
                      scalar(result, before);
                    } else {
                      equal(result.reason, 'budget', 'inactive or aligned access succeeds');
                      equal(result.alignmentCheck, undefined, 'no false architectural AC');
                      noOtherFault(result);
                      if (write)
                        wanted.set(hex(width === 2 ? 'beba' : 'bebafeca'), Number(address));
                      const low =
                        width === 2
                          ? address === 0x6000n
                            ? 0x2010n
                            : 0x3020n
                          : address === 0x6000n
                            ? 0x40302010n
                            : 0x50403020n;
                      const value =
                        cell.mode === 'long64' && width === 4
                          ? low
                          : (before.registers[0]! & (width === 2 ? ~0xffffn : ~0xffffffffn)) | low;
                      scalar(result, before, 1n, code.length / 2, write ? {} : { 0: value });
                    }
                    bytes(ram(), wanted, 'full RAM including payload and guards');
                    clock(cpu, fault ? 100n : 101n);
                    numeric();
                    same(cpu.readContext(), context, 'unaltered complete context');
                    if (controls)
                      same(cpu.readPagingState(), controls, 'CR2 and complete control bank');
                    equal(
                      result.deliveryAttempts,
                      undefined,
                      'report-only has no delivery counters',
                    );
                  }
              }
            });
          }
    },
  }));
}
