import { using as usingModel } from '../../../identity-time/oracle.js';
import { stack, ONE, banks } from '../../../numeric/x87/fixture.js';
import { cells } from './matrix.js';
import {
  AC,
  AUX,
  createCpu,
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

type Width = 2 | 4 | 8;
type Slot = { address: bigint; next: bigint; value: bigint };
type Pair = { aligned: Slot; misaligned: readonly Slot[] };
type Geometry = { bank: bigint; seed: bigint; slots: Partial<Record<Width, Pair>> };

// Literal stack cursors and little-endian loads, independent of stack lowering.
// Non-real S32 and S64 use different pages from S16. The S16 misaligned span
// crosses a 4KiB boundary; both pages are admitted by the independent model.
// Real S32 carries across 64KiB without reading beyond its segment limit.
const geometry: Record<'s16' | 's32' | 'real32' | 's64', Geometry> = {
  s16: {
    bank: 0x1122334455660000n,
    seed: 0x8ff8n,
    slots: {
      2: {
        aligned: { address: 0x8ffen, next: 0x9000n, value: 0x8070n },
        misaligned: [{ address: 0x8fffn, next: 0x9001n, value: 0x9080n }],
      },
      4: {
        aligned: { address: 0x8ffcn, next: 0x9000n, value: 0x80706050n },
        misaligned: [
          { address: 0x8ffdn, next: 0x9001n, value: 0x90807060n },
          { address: 0x8ffen, next: 0x9002n, value: 0xa0908070n },
          { address: 0x8fffn, next: 0x9003n, value: 0xb0a09080n },
        ],
      },
    },
  },
  s32: {
    bank: 0x1122334400000000n,
    seed: 0x18ff8n,
    slots: {
      2: {
        aligned: { address: 0x18ffen, next: 0x19000n, value: 0x8070n },
        misaligned: [{ address: 0x18fffn, next: 0x19001n, value: 0x9080n }],
      },
      4: {
        aligned: { address: 0x18ffcn, next: 0x19000n, value: 0x80706050n },
        misaligned: [
          { address: 0x18ffdn, next: 0x19001n, value: 0x90807060n },
          { address: 0x18ffen, next: 0x19002n, value: 0xa0908070n },
          { address: 0x18fffn, next: 0x19003n, value: 0xb0a09080n },
        ],
      },
    },
  },
  real32: {
    bank: 0x1122334400000000n,
    seed: 0xfff8n,
    slots: {
      2: {
        aligned: { address: 0xfffen, next: 0x10000n, value: 0x8070n },
        misaligned: [{ address: 0xfffdn, next: 0xffffn, value: 0x7060n }],
      },
      4: {
        aligned: { address: 0xfffcn, next: 0x10000n, value: 0x80706050n },
        misaligned: [
          { address: 0xfff9n, next: 0xfffdn, value: 0x50403020n },
          { address: 0xfffan, next: 0xfffen, value: 0x60504030n },
          { address: 0xfffbn, next: 0xffffn, value: 0x70605040n },
        ],
      },
    },
  },
  s64: {
    // S64 has no ignored upper SP bank: the whole RSP is the literal cursor.
    bank: 0n,
    seed: 0x28ff8n,
    slots: {
      2: {
        aligned: { address: 0x28ffen, next: 0x29000n, value: 0x8070n },
        misaligned: [{ address: 0x28fffn, next: 0x29001n, value: 0x9080n }],
      },
      8: {
        aligned: { address: 0x28ff8n, next: 0x29000n, value: 0x8070605040302010n },
        misaligned: [
          { address: 0x28ff9n, next: 0x29001n, value: 0x9080706050403020n },
          { address: 0x28ffan, next: 0x29002n, value: 0xa090807060504030n },
          { address: 0x28ffbn, next: 0x29003n, value: 0xb0a0908070605040n },
          { address: 0x28ffcn, next: 0x29004n, value: 0xc0b0a09080706050n },
          { address: 0x28ffdn, next: 0x29005n, value: 0xd0c0b0a090807060n },
          { address: 0x28ffen, next: 0x29006n, value: 0xe0d0c0b0a0908070n },
          { address: 0x28fffn, next: 0x29007n, value: 0xf0e0d0c0b0a09080n },
        ],
      },
    },
  },
};
const stored: Record<Width, string> = {
  2: 'beba',
  4: 'bebafeca',
  8: 'bebafeca44332211',
};

export function stackMatrixScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'alignment checking literal PUSH POP stack matrix across forty-three admitted geometry cells',
      async run() {
        equal(cells.length, 43, 'all independent stack geometry cells');
        let checked = 0,
          faults = 0;
        for (const cell of cells) {
          const family = cell.family!,
            long = cell.mode === 'long64',
            widths: readonly Width[] = long ? [2, 8] : [2, 4],
            key = long
              ? 's64'
              : cell.stack === 16
                ? 's16'
                : cell.mode === 'real'
                  ? 'real32'
                  : 's32',
            golden = geometry[key],
            profile = { ...cell, cpl: cell.mode === 'real' ? 0 : 3 };
          for (const width of widths)
            for (const push of [false, true]) {
              const row = golden.slots[width];
              if (!row) throw Error('missing independently authored stack width');
              const prefix = cell.bits === 16 ? (width === 4 ? '66' : '') : width === 2 ? '66' : '',
                code = prefix + (push ? '50' : '58');
              await usingModel({ createCpu, compiler }, code, profile, async (cpu) => {
                cpu.setX87State(stack({ 0: ONE }, 4));
                cpu.setXmmState(banks());
                cpu.setMxcsr(0x5f80);
                if (family !== 'linear') {
                  const bank = cpu.readPagingState();
                  cpu.setPagingState({ ...bank, cr0: bank.cr0 | AC });
                  equal(cpu.readPagingState().cr0 & AC, AC, 'stack AM round trip');
                }
                for (const [arm, slot] of [
                  ['aligned', row.aligned],
                  ...row.misaligned.map((slot) => ['misaligned', slot] as const),
                  ['inactive', row.misaligned.at(-1)!],
                ] as const) {
                  const ac = arm !== 'inactive',
                    beforeSp = golden.bank | (push ? slot.next : slot.address),
                    afterSp = golden.bank | (push ? slot.address : slot.next),
                    before = state(
                      { 4: beforeSp },
                      2n | (cell.mode === 'virtual8086' ? 0x3000n : 0n) | (ac ? AC : 0n),
                    );
                  if (family === 'linear') cpu.writeMemory(golden.seed, hex(payload));
                  else cpu.writePhysicalMemory(golden.seed, hex(payload));
                  const ram = () =>
                    family === 'linear'
                      ? cpu.readMemory(0n, 0x50000)
                      : cpu.readPhysicalMemory(0n, 0x50000);
                  cpu.setState(before);
                  cpu.setTimeState({ tsc: 100n, tscAux: AUX });
                  const wanted = ram(),
                    numeric = preserved(cpu),
                    context = cpu.readContext(),
                    controls = family === 'linear' ? undefined : cpu.readPagingState(),
                    result = await cpu.run({ budget: 1n, repeatBudget: 7n }),
                    fault = family !== 'linear' && cell.mode !== 'real' && arm === 'misaligned';
                  if (fault) {
                    checkFault(result, slot.address, width, push ? 'write' : 'read', width, 'ss');
                    scalar(result, before);
                    faults++;
                  } else {
                    equal(result.reason, 'budget', 'aligned or inactive stack operand succeeds');
                    equal(result.alignmentCheck, undefined, 'no false stack alignment report');
                    noOtherFault(result);
                    const changes: Record<number, bigint> = { 4: afterSp };
                    if (push) wanted.set(hex(stored[width]), Number(slot.address));
                    else
                      changes[0] =
                        width === 8
                          ? slot.value
                          : (before.registers[0]! & (width === 2 ? ~0xffffn : ~0xffffffffn)) |
                            slot.value;
                    scalar(result, before, 1n, code.length / 2, changes);
                  }
                  bytes(
                    ram(),
                    wanted,
                    `${family}/${cell.mode}/C${cell.bits}/${key}/W${width}/${arm} full stack RAM`,
                  );
                  numeric();
                  same(cpu.readContext(), context, 'stack context and segment caches');
                  if (controls) same(cpu.readPagingState(), controls, 'stack CR2 and control bank');
                  clock(cpu, fault ? 100n : 101n);
                  equal(result.deliveryAttempts, undefined, 'stack report-only event boundary');
                  checked++;
                }
              });
            }
        }
        equal(checked, 712, 'all stack residues plus one inactive arm for both directions');
        equal(faults, 216, 'all misaligned stack residues across twenty-five AC-eligible cells');
      },
    },
  ];
}
