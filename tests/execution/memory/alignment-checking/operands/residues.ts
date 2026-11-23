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
  memory,
  same,
  equal,
  hex,
  preserved,
  type Scenario,
} from '../fixture.js';

export function operandScenarios(compiler: WebAssembly.Module): Scenario[] {
  const side = { createCpu, compiler };
  return [
    {
      name: 'alignment checking every scalar residue reaches read write and implicit locked exchange paths',
      async run() {
        for (const family of families)
          for (const width of family === 'ia32e' || family === 'fixed' ? [2, 4, 8] : [2, 4])
            for (const kind of ['read', 'write', 'exchange'] as const) {
              const code =
                (width === 2 ? '66' : width === 8 ? '48' : '') +
                (kind === 'read' ? '8b03' : kind === 'write' ? '8903' : '8703');
              const f = fixture(compiler, code, family);
              await using(side, f, async (cpu) => {
                for (let residue = 0; residue < width; residue++) {
                  cpu.writePhysicalMemory(0xa000n, hex('102030405060708090a0b0c0d0e0f000'));
                  const at = 0x6000n + BigInt(residue),
                    before = state({ 3: at }),
                    wanted = f.bytes.slice(),
                    unchanged = preserved(cpu);
                  const result = await runOne(cpu, before);
                  if (residue) {
                    checkFault(result, at, width, kind === 'read' ? 'read' : 'write', width);
                    scalar(result, before);
                  } else {
                    equal(result.reason, 'budget', 'zero residue succeeds');
                    equal(result.alignmentCheck, undefined, 'no false aligned AC');
                    if (kind !== 'read')
                      wanted.set(
                        hex(width === 2 ? 'beba' : width === 4 ? 'bebafeca' : 'bebafeca44332211'),
                        0xa000,
                      );
                    const low =
                      width === 2 ? 0x2010n : width === 4 ? 0x40302010n : 0x8070605040302010n;
                    const value =
                      width === 8 || (width === 4 && (family === 'ia32e' || family === 'fixed'))
                        ? low
                        : (before.registers[0]! & (width === 2 ? ~0xffffn : ~0xffffffffn)) | low;
                    scalar(
                      result,
                      before,
                      1n,
                      code.length / 2,
                      kind === 'write' ? {} : { 0: value },
                    );
                  }
                  unchanged();
                  memory(cpu, wanted, 'exact residue-specific scalar payload');
                }
              });
            }
      },
    },
    {
      name: 'alignment checking selector queries and far loads use actual words and pointer offset granules',
      async run() {
        for (const code of ['0f0203', '0f0303', '0f0023', '0f002b', '8e1b']) {
          const f = fixture(compiler, code);
          await using(side, f, async (cpu) => {
            const before = state(),
              context = cpu.readContext(),
              result = await runOne(cpu, before);
            checkFault(result, 0x6001n, 2, 'read', 2);
            scalar(result, before);
            same(cpu.readContext(), context, 'no selector/cache candidate');
            memory(cpu, f.bytes, 'selector refusal no descriptor writes');
          });
        }
        for (const code of ['0f0003', '0f000b'])
          for (const odd of [false, true]) {
            const f = fixture(compiler, code),
              at = odd ? 0x6001n : 0x6002n;
            await using(side, f, async (cpu) => {
              const before = state({ 3: at }),
                wanted = f.bytes.slice(),
                result = await runOne(cpu, before);
              if (odd) {
                checkFault(result, at, 2, 'write', 2);
                scalar(result, before);
              } else {
                equal(result.reason, 'budget', 'selected SLDT STR word profile');
                scalar(result, before, 1n, 3);
                wanted.set(hex('0000'), 0xa002);
              }
              memory(cpu, wanted, 'word selector publication never widens');
            });
          }
        for (const small of [false, true]) {
          const code = small ? '66c503' : 'c503',
            at = small ? 0x6002n : 0x6004n,
            f = fixture(compiler, code);
          f.bytes.set(hex('ffff000000f3cf00'), 0xb010);
          f.bytes.set(hex(small ? '78561300' : '785634121300'), 0xa000 + Number(at - 0x6000n));
          await using(side, f, async (cpu) => {
            const before = state({ 3: at }),
              result = await runOne(cpu, before);
            equal(result.reason, 'budget', 'far pointer aligns offset component not total span');
            scalar(result, before, 1n, code.length / 2, {
              0: small ? 0x11223344cafe5678n : 0x1122334412345678n,
            });
            equal(cpu.readContext().segments.ds.selector, 0x13, 'actual DS load');
            memory(cpu, f.bytes, 'already-accessed descriptor unchanged');
          });
        }
      },
    },
    {
      name: 'alignment checking bit strings report the adjusted unit rather than the unadjusted base',
      async run() {
        for (const write of [false, true]) {
          const f = fixture(compiler, write ? '0fab0b' : '0fa30b');
          await using(side, f, async (cpu) => {
            const before = state({ 1: 33n }),
              result = await runOne(cpu, before);
            checkFault(result, 0x6005n, 4, write ? 'write' : 'read', 4);
            scalar(result, before);
            memory(cpu, f.bytes, 'adjusted bit-string refusal no flags/data');
          });
        }
      },
    },
    {
      name: 'alignment checking POPF reads under old AC and sixteen-bit restores cannot clear the upper flag',
      async run() {
        for (const kind of ['old-on', 'enable', 'word'] as const) {
          const code = kind === 'word' ? '669d8b03' : '9d8b03',
            f = fixture(compiler, code);
          const sp = kind === 'word' ? 0x9000n : 0x9001n;
          f.bytes.set(
            hex(kind === 'enable' ? '02000400' : '02000000'),
            0xd000 + Number(sp - 0x9000n),
          );
          await using(side, f, async (cpu) => {
            const before = state({ 4: sp }, kind === 'enable' ? 2n : AC | 2n);
            cpu.setState(before);
            cpu.setTimeState({ tsc: 100n, tscAux: AUX });
            const result = await cpu.run({ budget: 2n, repeatBudget: 7n });
            if (kind === 'old-on') {
              checkFault(result, 0x9001n, 4, 'read', 4, 'ss');
              same(result.state, before, 'candidate flags not used for their own stack');
              equal(result.retired, 0n, 'refused POPF no retirement');
            } else {
              const pc = kind === 'word' ? PC + 2n : PC + 1n;
              same(
                result.alignmentCheck,
                {
                  instructionPc: pc,
                  vector: 17,
                  errorCode: 0,
                  segment: 'ds',
                  offset: 0x6001n,
                  address: 0x6001n,
                  width: 4,
                  access: 'read',
                  alignment: 4,
                },
                'subsequent access uses restored or preserved live AC',
              );
              equal(result.reason, 'alignment-check', 'later operand AC');
              equal(result.retired, 1n, 'successful POPF exactly once');
              const expected = {
                ...before,
                rip: pc,
                flags: AC | 2n,
                registers: [...before.registers],
              };
              expected.registers[4] = kind === 'word' ? 0x9002n : 0x9005n;
              same(result.state, expected, 'terminal flag restore resumes correct context');
            }
            memory(cpu, f.bytes, 'POPF and refused load no data publication');
          });
        }
      },
    },
  ];
}
