import type { Cpu } from '../../../host/index.js';
import {
  DATA,
  PC,
  createCpu,
  fixture,
  key,
  pageFault,
  pair,
  same,
  state,
  using as usingPhysical,
} from '../protection-keys/fixture.js';
import {
  OTHER_MXCSR,
  banks,
  changed,
  context,
  equal,
  hex,
  input,
  outcome,
  profiles,
  seed,
  stack,
  using,
  x87,
  xmm,
  type Scenario,
} from '../x87/fixture.js';

const source = 'ff807f00fe81020310203040f0e0d0c0';
const absolute = 0x403020104030201003027f02007f8001n;

function seedBanks(cpu: Cpu, wide: boolean) {
  // XMM integer execution is independent of a pending x87 exception.
  const numeric = wide ? stack({}, 5, 0x37b, 0x8084) : stack({}, 5);
  seed(cpu, numeric);
  return numeric;
}

function checkResult(cpu: Cpu, wide: boolean, numeric: ReturnType<typeof stack>) {
  const vector = banks();
  if (wide) vector[0] = absolute;
  else {
    numeric.registers[0] = (0xffffn << 64n) | BigInt.asUintN(64, absolute);
    numeric.tagWord = 0;
    numeric.statusWord &= ~0x3800;
  }
  x87(cpu, numeric);
  xmm(cpu, vector);
  equal(cpu.readMxcsr(), OTHER_MXCSR, 'PABSB preserves MXCSR');
}

export function ssse3BoundaryScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'SSSE3 uses the FS base and effective address size across real, protected, virtual8086 and compatibility modes',
      async run() {
        for (const { mode, code32, stack32 } of profiles) {
          if (mode === 'long64' || stack32 !== code32) continue;
          const execution = context(mode, code32, stack32);
          execution.segments.fs.base = 0x5000n;
          for (const override of [false, true]) {
            const addr16 = code32 === override;
            for (const wide of [false, true]) {
              const code =
                '64' +
                (override ? '67' : '') +
                (wide ? '66' : '') +
                '0f381c' +
                (addr16 ? '07' : '00');
              // FS:[BX] or FS:[EAX]. Each address size must discard the high bits.
              const before = input(PC, {
                [addr16 ? 3 : 0]: addr16 ? 0x123456781000n : 0x123400001000n,
              });
              await using(
                compiler,
                code,
                execution,
                async (cpu) => {
                  const numeric = seedBanks(cpu, wide);
                  cpu.setState(before);
                  outcome(
                    await cpu.run({ budget: 1n }),
                    'budget',
                    1n,
                    1n,
                    1n,
                    0n,
                    changed(before, PC + BigInt(code.length / 2)),
                  );
                  checkResult(cpu, wide, numeric);
                },
                { data: [{ base: DATA, bytes: source, write: false }] },
              );
            }
          }
        }
      },
    },
    {
      name: 'SSSE3 cached middle entry rechecks live PKRU before publishing an MMX or XMM destination and read watchpoint',
      async run() {
        for (const wide of [false, true]) {
          // MOV AL,[RBX] warms the data translation without touching its watched last byte.
          const code = '8a03' + (wide ? '66' : '') + '0f381c03';
          const f = fixture(compiler, code, { cpl: 3 });
          f.leaf(Number(DATA), key(3) | 0xa067n);
          f.bytes.set(hex(source), 0xa000);
          await usingPhysical({ createCpu, compiler }, f, async (cpu) => {
            const numeric = seedBanks(cpu, wide);
            cpu.setDebugState({
              addresses: [DATA + (wide ? 15n : 7n), 0n, 0n, 0n],
              dr7: 0x30401n,
              dr6: 0n,
            });
            cpu.setState(state());
            const warm = await cpu.run({ budget: 1n });
            equal(warm.reason, 'budget', 'first byte read retires');
            equal(warm.state.rip, PC + 2n, 'following middle entry');
            cpu.setProtectionKeysState({ pkru: pair(3, 1), pkrs: 0 });
            const denied = await cpu.run({ budget: 1n });
            pageFault(denied, DATA, 'read', 0x25, PC + 2n);
            equal(denied.resources.compilations, 0, 'reuses compiled middle entry');
            equal(denied.retired, 0n, 'denied source does not retire');
            equal(cpu.readPagingState().cr2, DATA, 'CR2 latches denied linear address');
            same(denied.state, warm.state, 'unchanged GPRs, RIP, flags and event state');
            equal(cpu.readDebugState().dr6 & 15n, 0n, 'denial leaves breakpoint status clear');
            x87(cpu, numeric);
            xmm(cpu, banks());
            equal(cpu.readMxcsr(), OTHER_MXCSR, 'denial preserves MXCSR');

            cpu.setProtectionKeysState({ pkru: 0, pkrs: 0 });
            const repaired = await cpu.run({ budget: 1n });
            equal(repaired.reason, 'debug-exception', 'read watchpoint published after repair');
            equal(repaired.retired, 1n, 'repaired PABSB retires exactly once');
            equal(repaired.state.rip, PC + BigInt(code.length / 2), 'read trap saves next RIP');
            equal(repaired.debugException?.breakpoints, 1, 'repaired source hits slot zero');
            equal(cpu.readDebugState().dr6 & 15n, 1n, 'accepted read publishes DR6');
            checkResult(cpu, wide, numeric);
          });
        }
      },
    },
  ];
}
