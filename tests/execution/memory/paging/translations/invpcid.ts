import { createCpu } from '../../../../../sdk/src/index.js';
import { hex } from '../../../../support/fixture.js';
import { context as legacyContext } from '../../../modes/fixture.js';
import { RESET_BANK, resetContext } from '../../../reset/fixture.js';
import {
  AC,
  AUX,
  DATA,
  PC,
  SMAP,
  families,
  fixture,
  context,
  memory,
  preserved,
  put,
  same,
  state,
  using,
  word,
  type Family,
  type Scenario,
} from '../../supervisor-access/fixture.js';

const modes: readonly (readonly [Family, 16 | 32 | 64])[] = [
  ['non-pae', 16],
  ['non-pae', 32],
  ['pae', 16],
  ['pae', 32],
  ['ia32e', 16],
  ['ia32e', 32],
  ['ia32e', 64],
  ['fixed', 16],
  ['fixed', 32],
  ['fixed', 64],
];

export function invpcidScenarios(compiler: WebAssembly.Module): Scenario[] {
  const side = { createCpu, compiler };
  return [
    {
      name: 'INVPCID all types preserve scalar numeric time and raw banks across every paging family and code width',
      async run() {
        for (const [family, bits] of modes)
          for (const kind of [0n, 1n, 2n, 3n]) {
            const code = (bits === 16 ? '67' : '') + '66660f388203';
            const f = fixture(side, family, code, { bits });
            f.paging.cr0 |= 0x4000cn;
            f.paging.cr4 &= ~0x600n;
            put(f.bytes, 0xa001, 0n);
            put(f.bytes, 0xa009, kind === 0n ? 0xffff800000004000n : 0x800000000000n);
            await using(side, f, async (cpu) => {
              const before = state(2n | AC);
              before.registers[0] = kind | (bits === 64 ? 0n : 0x1234567800000000n);
              before.registers[3] = DATA + 1n;
              cpu.setState(before);
              cpu.setTimeState({ tsc: 100n, tscAux: AUX });
              const numeric = preserved(cpu);
              const result = await cpu.run({ budget: 1n });
              same(result.reason, 'budget', `${family}/${bits}/${kind} completion`);
              same(result.retired, 1n, 'single completion');
              same(
                result.state,
                { ...before, rip: PC + BigInt(code.length / 2) },
                'unchanged operands and flags',
              );
              same(cpu.readPagingState(), f.paging, 'invalidation does not write controls');
              same(cpu.readTimeState(), { tsc: 101n, tscAux: AUX }, 'one completed tick');
              memory(cpu, f.bytes, 'descriptor read and absent target do not write RAM');
              numeric();
            });
          }
      },
    },
    {
      name: 'INVPCID real-address mode executes while VM86 and user privilege precede invalid type and descriptor admission',
      async run() {
        for (const kind of [0n, 1n, 2n, 3n]) {
          const f = fixture(side, 'non-pae', '', { bits: 16 });
          const execution = resetContext();
          execution.segments.cs.selector = 0;
          execution.segments.cs.base = 0n;
          f.options.context = execution;
          f.options.paging = { ...RESET_BANK, cr2: 0xdeadn };
          f.bytes.set(hex('67660f388203'), 0x1000);
          put(f.bytes, 0x6001, 0n);
          put(f.bytes, 0x6009, kind === 0n ? 0xffff800000004000n : 0x800000000000n);
          await using(side, f, async (cpu) => {
            const before = state();
            before.registers[0] = 0x1234567800000000n | kind;
            before.registers[3] = 0x6001n;
            cpu.setState(before);
            const result = await cpu.run({ budget: 1n });
            same(result.reason, 'budget', 'real mode executes INVPCID');
            same(result.state, { ...before, rip: PC + 6n }, 'real scalar preservation');
            same(cpu.readPagingState().cr2, 0xdeadn, 'real CR2 preservation');
          });
        }
        for (const vm of [false, true]) {
          const f = fixture(side, 'non-pae', '67660f388203', { bits: 16, cpl: 3 });
          if (vm) f.options.context = legacyContext('virtual8086', false, false);
          await using(side, f, async (cpu) => {
            const before = state(2n);
            before.registers[0] = 4n;
            before.registers[3] = 0xffffffffn;
            cpu.setState(before);
            const result = await cpu.run({ budget: 1n });
            same(result.reason, 'protection-fault', 'privileged instruction');
            same(result.retired, 0n, 'failed privilege does not retire');
            same(
              result.protectionFault,
              { vector: 13, errorCode: 0, phase: 'privilege', instructionPc: PC },
              'privilege wins',
            );
            same(cpu.readPagingState().cr2, 0xdeadn, 'no descriptor paging');
          });
        }
      },
    },
    {
      name: 'INVPCID descriptor reserved canonical ignored and disabled-PCID fields retain their exact fault contract',
      async run() {
        for (const family of families)
          for (const [kind, low, high, success] of [
            [0n, 0x1000n, 0n, false],
            [1n, 0x1000n, 0n, false],
            [2n, 0x1000n, 0n, false],
            [3n, 0x8000000000000000n, 0n, false],
            [0n, 1n, 0n, false],
            [1n, 1n, 0n, false],
            [2n, 0xfffn, 0x800000000000n, true],
            [3n, 0xfffn, 0x800000000000n, true],
            [0n, 0n, 0x800000000000n, false],
            [1n, 0n, 0x800000000000n, true],
          ] as const) {
            const f = fixture(side, family, '660f388203');
            put(f.bytes, 0xa000, low);
            put(f.bytes, 0xa008, high);
            await using(side, f, async (cpu) => {
              const before = state();
              before.registers[0] = kind;
              cpu.setState(before);
              const result = await cpu.run({ budget: 1n });
              same(result.reason, success ? 'budget' : 'protection-fault', 'descriptor content');
              same(result.retired, success ? 1n : 0n, 'content retirement');
              same(
                result.state,
                { ...before, rip: success ? PC + 5n : PC },
                'faulting descriptor preserves scalar',
              );
              if (!success)
                same(
                  result.protectionFault,
                  { vector: 13, errorCode: 0, phase: 'target', instructionPc: PC },
                  'target content fault',
                );
              same(cpu.readPagingState().cr2, 0xdeadn, 'content fault preserves CR2');
              memory(cpu, f.bytes, 'descriptor read only');
            });
          }
      },
    },
    {
      name: 'INVPCID register type validation wins before noncanonical descriptor address in long64',
      async run() {
        for (const kind of [4n, 0x100000000n]) {
          const f = fixture(side, 'fixed', '660f388203');
          await using(side, f, async (cpu) => {
            const before = state();
            before.registers[0] = kind;
            before.registers[3] = 0x800000000000n;
            cpu.setState(before);
            const result = await cpu.run({ budget: 1n });
            same(result.reason, 'protection-fault', 'type before descriptor EA');
            same(
              result.protectionFault,
              { vector: 13, errorCode: 0, phase: 'target', instructionPc: PC },
              'selected simultaneous priority',
            );
            same(result.retired, 0n, 'invalid full64 type');
            same(result.state, before, 'no scalar changes');
            same(cpu.readPagingState().cr2, 0xdeadn, 'no descriptor PF');
          });
        }
      },
    },
    {
      name: 'INVPCID descriptor EA keeps SS versus GP provenance before paging and private access',
      async run() {
        for (const stack of [false, true]) {
          for (const family of ['non-pae', 'fixed'] as const) {
            const f = fixture(side, family, stack ? '660f38824500' : '660f388203');
            if (family === 'non-pae') {
              const execution = context(family);
              execution.segments[stack ? 'ss' : 'ds'].limit = 0x6007;
              f.options.context = execution;
              f.leaf(0x6000, 0n);
            }
            await using(side, f, async (cpu) => {
              const before = state();
              before.registers[0] = 2n;
              before.registers[stack ? 5 : 3] = family === 'fixed' ? 0x7ffffffffff8n : DATA;
              cpu.setState(before);
              const result = await cpu.run({ budget: 1n, eventDelivery: { budget: 0n } });
              same(result.reason, 'event-budget', 'architectural operand-address fault');
              same(result.retired, 0n, 'faulting operand does not retire');
              same(result.state.rip, PC, 'faulting instruction PC');
              same(cpu.readPagingState().cr2, 0xdeadn, 'address fault precedes page walk');
              if (result.state.event?.kind !== 'pending')
                throw Error('missing selected operand exception');
              same(
                result.state.event.exception.vector,
                stack ? 12 : 13,
                `${family}/${stack}: SS versus other descriptor EA`,
              );
              same(result.state.event.exception.errorCode, 0, 'operand-address error code');
            });
          }
        }
      },
    },
    {
      name: 'INVPCID explicit descriptor observes SMAP AC and its complete split span even for all-context types',
      async run() {
        for (const family of families)
          for (const ac of [0n, AC]) {
            const f = fixture(side, family, '660f388203');
            f.paging.cr4 |= SMAP;
            f.leaf(0x6000, 0xa067n);
            put(f.bytes, 0xa000, 0n);
            put(f.bytes, 0xa008, 0n);
            await using(side, f, async (cpu) => {
              const before = state(2n | ac);
              before.registers[0] = 2n;
              cpu.setState(before);
              const result = await cpu.run({ budget: 1n });
              same(result.reason, ac ? 'budget' : 'page-fault', 'explicit raw AC admission');
              same(result.retired, ac ? 1n : 0n, 'descriptor admission retirement');
              if (!ac)
                same(
                  result.pageFault,
                  { vector: 14, address: DATA, access: 'read', errorCode: 1, instructionPc: PC },
                  'SMAP explicit read',
                );
            });
          }
        for (const privateRefusal of [false, true]) {
          const f = fixture(side, 'fixed', '660f388203');
          put(f.bytes, 0xaff8, 0x1000n);
          if (privateRefusal)
            f.options.memory!.mappings = [
              {
                base: 0n,
                backing: 'ram',
                offset: 0,
                length: 0xb000,
                permissions: { read: true, write: true, execute: true },
              },
              {
                base: 0xb008n,
                backing: 'ram',
                offset: 0xb008,
                length: f.bytes.length - 0xb008,
                permissions: { read: true, write: true, execute: true },
              },
            ];
          else f.leaf(0x7000, 0n);
          await using(side, f, async (cpu) => {
            const before = state();
            before.registers[0] = 2n;
            before.registers[3] = 0x6ff8n;
            cpu.setState(before);
            const result = await cpu.run({ budget: 1n });
            same(
              result.reason,
              privateRefusal ? 'physical-memory-fault' : 'page-fault',
              'ignored descriptor address still read',
            );
            same(result.retired, 0n, 'split descriptor refusal');
            same(result.state, before, 'no scalar publication');
            same(
              cpu.readPagingState().cr2,
              privateRefusal ? 0xdeadn : 0x7000n,
              'precise descriptor CR2',
            );
            if (privateRefusal)
              same(result.physicalMemoryFault?.phase, 'final-access', 'second physical span');
          });
        }
      },
    },
    {
      name: 'INVPCID all four types invalidate a retained guest PTE view after a guest-only retarget',
      async run() {
        for (const family of families)
          for (const kind of [0n, 1n, 2n, 3n]) {
            const wide = family === 'ia32e' || family === 'fixed';
            const code = '8b13' + (wide ? '48' : '') + '89378b33660f3882018b2b';
            const f = fixture(side, family, code);
            f.leaf(0x5000, BigInt(f.pt) | 0x63n);
            put(f.bytes, 0xa000, 0x44332211n, 4);
            put(f.bytes, 0x18000, 0x88776655n, 4);
            put(f.bytes, 0xb000, 0n);
            put(f.bytes, 0xb008, DATA);
            await using(side, f, async (cpu) => {
              const before = state();
              before.registers[0] = kind;
              before.registers[1] = 0x7000n;
              before.registers[6] = 0x18063n;
              before.registers[7] = 0x5000n + BigInt(6 * f.width);
              cpu.setState(before);
              const result = await cpu.run({ budget: 5n });
              same(result.reason, 'budget', 'complete guest-only retarget sequence');
              same(result.retired, 5n, 'five logical operations');
              same(result.state.registers[2]! & 0xffffffffn, 0x44332211n, 'initial retained value');
              same(
                result.state.registers[6]! & 0xffffffffn,
                0x44332211n,
                'guest PTE write alone retains old value',
              );
              same(
                result.state.registers[5]! & 0xffffffffn,
                0x88776655n,
                'INVPCID exposes new value',
              );
              same(cpu.readPagingState().cr2, 0xdeadn, 'no target walk fault');
            });
          }
      },
    },
    {
      name: 'INVPCID descriptor read traps only after successful invalidation and drops hits on content faults',
      async run() {
        for (const valid of [false, true]) {
          const f = fixture(side, 'fixed', '660f38820390');
          put(f.bytes, 0xa000, valid ? 0n : 0x1000n);
          put(f.bytes, 0xa008, 0n);
          await using(side, f, async (cpu) => {
            const before = state();
            before.registers[0] = 2n;
            cpu.setState(before);
            cpu.setDebugState({ addresses: [DATA, 0n, 0n, 0n], dr6: 0n, dr7: 0x30401n });
            const result = await cpu.run({ budget: 1n, stop: PC + 5n });
            same(
              result.reason,
              valid ? 'debug-exception' : 'protection-fault',
              'data trap after successful instruction',
            );
            same(result.retired, valid ? 1n : 0n, 'precise completion');
            same(result.state.rip, valid ? PC + 5n : PC, 'fault or completed resume PC');
            same(
              cpu.readDebugState().dr6,
              valid ? 0xffff0ff1n : 0xffff0ff0n,
              'only completed operand hit',
            );
            if (!valid) {
              cpu.setState({ ...result.state, rip: PC + 5n });
              same(
                (await cpu.run({ budget: 1n })).reason,
                'budget',
                'faulted read hit cannot leak to NOP',
              );
            }
          });
        }
      },
    },
    {
      name: 'INVPCID self-source invalidation completes the current instruction once and enters the replacement successor',
      async run() {
        const oldCode = '660f388203b801000000f4';
        const newCode = '9090909090b802000000f4';
        const f = fixture(side, 'fixed', '');
        f.bytes.fill(0x82, 0x19000, 0x1c000);
        f.bytes.set(hex(oldCode), 0x19000);
        f.bytes[0x19100] = 0x90;
        f.bytes.set(hex(newCode), 0x1a000);
        f.bytes.set(hex('488937'), 0x1b000);
        f.leaf(0x1000, 0x19007n);
        f.leaf(0x2000, 0x1b063n);
        f.leaf(0x5000, BigInt(f.pt) | 0x63n);
        put(f.bytes, 0xa000, 0n);
        put(f.bytes, 0xa008, 0x7fffffffffffn);
        await using(side, f, async (cpu) => {
          const before = state(2n, PC + 0x100n);
          cpu.setState(before);
          cpu.setTimeState({ tsc: 100n, tscAux: AUX });
          const warm = await cpu.run({ budget: 1n });
          same(warm.reason, 'budget', 'required NOP fills the old source page');
          const writer: typeof warm.state = {
            ...warm.state,
            rip: 0x2000n,
            registers: [...warm.state.registers],
          };
          writer.registers[6] = 0x1a007n;
          writer.registers[7] = 0x5000n + BigInt(f.width);
          cpu.setState(writer);
          const retarget = await cpu.run({ budget: 1n });
          same(retarget.reason, 'budget', 'guest-only code PTE retarget');
          same(word(cpu, f.pt + f.width), 0x1a007n, 'replacement source has not been walked');
          const input: typeof retarget.state = {
            ...retarget.state,
            rip: PC,
            registers: [...retarget.state.registers],
          };
          input.registers[0] = 2n;
          input.registers[3] = DATA;
          cpu.setState(input);
          const first = await cpu.run({ budget: 1n });
          same(first.reason, 'budget', 'retained INVPCID completes');
          same(first.retired, 1n, 'invalidation retires exactly once');
          same(first.state.rip, PC + 5n, 'old admitted INVPCID, not replacement NOP');
          same(
            word(cpu, f.pt + f.width),
            0x1a007n,
            'invalidation does not fetch the replacement source early',
          );
          const second = await cpu.run({ budget: 1n });
          same(second.reason, 'budget', 'replacement successor completes');
          same(second.retired, 1n, 'no invalidation replay');
          same(second.state.rip, PC + 10n, 'new successor length');
          same(second.state.registers[0], 2n, 'new MOV2 instead of stale MOV1');
          same(word(cpu, f.pt + f.width), 0x1a027n, 'reached successor establishes its own A');
          same(
            cpu.readPhysicalMemory(0x19000n, oldCode.length / 2),
            hex(oldCode),
            'old source bytes unchanged',
          );
          same(
            cpu.readTimeState(),
            { tsc: 104n, tscAux: AUX },
            'warm, guest write, invalidation and successor each retire once',
          );
        });
      },
    },
    {
      name: 'INVPCID content fault keeps the completed descriptor fill and discards debug before guest-only retarget',
      async run() {
        const f = fixture(side, 'fixed', '660f388203');
        f.leaf(0x6000, 0x16003n);
        f.leaf(0x2000, 0x1b063n);
        f.leaf(0x3000, 0x1c063n);
        f.leaf(0x5000, BigInt(f.pt) | 0x63n);
        f.bytes.set(hex('00100000000000000040000000000000'), 0x16000);
        put(f.bytes, 0x17000, 0x88776655n, 4);
        f.bytes.fill(0x82, 0x1b000, 0x1d000);
        f.bytes.set(hex('488937'), 0x1b000);
        f.bytes.set(hex('8b03'), 0x1c000);
        await using(side, f, async (cpu) => {
          const before = state();
          before.registers[0] = 2n;
          cpu.setState(before);
          cpu.setTimeState({ tsc: 100n, tscAux: AUX });
          cpu.setDebugState({ addresses: [DATA, 0n, 0n, 0n], dr6: 0xffff0ff0n, dr7: 0xf0401n });
          const fault = await cpu.run({ budget: 1n });
          same(fault.reason, 'protection-fault', 'reserved descriptor content');
          same(
            fault.protectionFault,
            { vector: 13, errorCode: 0, phase: 'target', instructionPc: PC },
            'full descriptor GP',
          );
          same(fault.retired, 0n, 'fault does not retire or invalidate');
          same(fault.state, before, 'faulting scalar state');
          same(cpu.readDebugState().dr6, 0xffff0ff0n, 'fault discards the descriptor hit');
          same(fault.state.debugContinuation, undefined, 'no durable failed-read candidate');
          same(
            word(cpu, f.pt + 6 * f.width),
            0x16023n,
            'completed descriptor walk establishes A without D',
          );
          const writer: typeof fault.state = {
            ...fault.state,
            rip: 0x2000n,
            registers: [...fault.state.registers],
          };
          writer.registers[6] = 0x17003n;
          writer.registers[7] = 0x5000n + BigInt(6 * f.width);
          cpu.setState(writer);
          const retarget = await cpu.run({ budget: 1n });
          same(retarget.reason, 'budget', 'unwatched guest PTE store has no leaked data trap');
          same(retarget.retired, 1n, 'one guest-only retarget');
          same(
            cpu.readDebugState().dr6,
            0xffff0ff0n,
            'faulted descriptor hit does not leak into writer',
          );
          same(word(cpu, f.pt + 6 * f.width), 0x17003n, 'guest replacement is unaccessed');
          cpu.setState({ ...retarget.state, rip: 0x3000n });
          const read = await cpu.run({ budget: 1n });
          same(read.reason, 'debug-exception', 'only the new completed read traps');
          same(read.retired, 1n, 'ordinary retained read completes once');
          same(read.state.rip, 0x3002n, 'new read resume');
          same(read.state.registers[0], 0x1000n, 'content GP preserved the old descriptor fill');
          same(read.debugException?.instructionPc, 0x3000n, 'trap belongs to the later read');
          same(read.debugException?.breakpoints, 1, 'fresh read hit only');
          same(
            word(cpu, f.pt + 6 * f.width),
            0x17003n,
            'retained read does not walk replacement B',
          );
          same(cpu.readPagingState().cr2, 0xdeadn, 'no invented PF');
          same(
            cpu.readTimeState(),
            { tsc: 102n, tscAux: AUX },
            'failed descriptor plus two completed guest operations',
          );
        });
      },
    },
    {
      name: 'INVPCID selected opcode prefix and illegal-form frontiers precede unavailable following fetch bytes',
      async run() {
        for (const [code, expected] of [
          ['0f38', 'page-fault'],
          ['0f3882', 'unsupported'],
          ['f2660f38', 'unsupported'],
          ['66f30f38', 'unsupported'],
          ['660f3881', 'unsupported'],
          ['66f00f3882', 'invalid-encoding'],
          ['660f3882c0', 'invalid-encoding'],
          ['660f38', 'page-fault'],
          ['660f3882', 'page-fault'],
          ['660f388204', 'page-fault'],
        ] as const) {
          const f = fixture(side, 'fixed', '');
          const rip = 0x2000n - BigInt(code.length / 2);
          f.bytes.set(hex(code), 0x9000 - code.length / 2);
          f.leaf(0x2000, 0n);
          await using(side, f, async (cpu) => {
            const before = state(2n, rip);
            cpu.setState(before);
            const result = await cpu.run({ budget: 1n });
            same(result.reason, expected, `required frontier ${code}`);
            same(result.retired, 0n, 'frontier does not retire');
            same(result.state, before, 'frontier scalar preservation');
            same(
              cpu.readPagingState().cr2,
              expected === 'page-fault' ? 0x2000n : 0xdeadn,
              'only required byte produces PF',
            );
          });
        }
      },
    },
  ];
}
