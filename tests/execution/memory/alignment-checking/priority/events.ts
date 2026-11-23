import type { EventState, ExecutionContextSnapshot, State } from '../../../../../sdk/src/index.js';
import { gate } from '../../../software-events/fixture.js';
import {
  AC,
  AUX,
  PC,
  createCpu,
  fixture,
  using,
  state,
  hex,
  same,
  equal,
  memory,
  clock,
  type Scenario,
} from '../fixture.js';

function eventFixture(compiler: WebAssembly.Module) {
  const f = fixture(compiler, '8b03');
  // The first misaligned7ffd dword reaches8000 before natural checking.
  f.leaf(0x8000, 0xc067n);
  // Cached source selectors0b/13 and these independently authored descriptors agree.
  f.bytes.set(hex('ffff000000fbcf00ffff000000f3cf00ffff0000009bcf00ffff00000093cf00'), 0xb008);
  for (const vector of [2, 17]) f.bytes.set(hex(gate(32, 0xb, 0x4000n, 3)), 0xc000 + vector * 8);
  f.bytes.set(hex('83c404cf'), 0x9000);
  return f;
}

const pending = (errorCode: 0 | 1): Extract<EventState, { kind: 'pending' }> => ({
  kind: 'pending',
  exception: { vector: 17, instructionPc: PC, returnPc: PC, savedFlags: 0x50002n, errorCode },
});

export function eventScenarios(compiler: WebAssembly.Module): Scenario[] {
  const side = { createCpu, compiler };
  return [
    {
      name: 'alignment checking IA32e rounds an odd same-CPL user RSP before its literal six-qword frame',
      async run() {
        const f = fixture(compiler, '8b03', 'fixed');
        f.leaf(0x8000, 0xc067n);
        f.bytes.set(hex('ffff000000fbaf00ffff000000f3cf00'), 0xb008);
        f.bytes.set(hex(gate(64, 0xb, 0x4000n, 3)), 0xc000 + 17 * 16);
        await using(side, f, async (cpu) => {
          const before = state({ 4: 0x9003n });
          cpu.setState(before);
          cpu.setTimeState({ tsc: 100n, tscAux: AUX });
          const result = await cpu.run({
            budget: 1n,
            stop: 0x4000n,
            eventDelivery: { budget: 1n },
          });
          equal(result.reason, 'stop', 'rounded same-CPL3 long frame admitted');
          equal(result.retired, 0n, 'source fault and automatic entry no retirement');
          equal(result.deliveryAttempts, 1n, 'one long entry');
          equal(cpu.readContext().cpl, 3, 'same prospective user CPL');
          equal(result.state.registers[4], 0x8fd0n, 'RSP9003 rounds9000 then six slots');
          same(
            cpu.readPhysicalMemory(0xcfd0n, 48),
            hex(
              '000000000000000000100000000000000b00000000000000020005000000000003900000000000001300000000000000',
            ),
            'independent six-qword error RF oldRSP SS literal',
          );
          clock(cpu);
        });
      },
    },
    {
      name: 'alignment checking stages error zero RF without a diagnostic checkpoint bank and enters a literal user frame',
      async run() {
        const f = eventFixture(compiler);
        await using(side, f, async (cpu) => {
          const before = state({ 4: 0x8000n });
          cpu.setState(before);
          cpu.setTimeState({ tsc: 100n, tscAux: AUX });
          const staged = await cpu.run({ budget: 1n, eventDelivery: { budget: 0n } });
          equal(staged.reason, 'event-budget', 'staged source without entry');
          equal(staged.retired, 0n, 'source AC no retirement');
          equal(staged.deliveryAttempts, 0n, 'capture is not entry');
          same(staged.state, { ...before, event: pending(0) }, 'plain pending vector code PC RF');
          memory(cpu, f.bytes, 'no frame before allowance');
          cpu.setPagingState({ ...cpu.readPagingState(), cr0: cpu.readPagingState().cr0 & ~AC });
          same(cpu.readState().event, pending(0), 'captured source not recomputed after AM clear');
          const entered = await cpu.run({
            budget: 1n,
            stop: 0x4000n,
            eventDelivery: { budget: 1n },
          });
          equal(entered.reason, 'stop', 'actual handler reached');
          equal(entered.retired, 0n, 'automatic entry no instruction');
          equal(entered.deliveryAttempts, 1n, 'one automatic entry');
          same(
            cpu.readPhysicalMemory(0xbff0n, 16),
            hex('00000000001000000b00000002000500'),
            'literal error0 IP CS savedAC RF frame',
          );
          same(
            entered.state,
            {
              ...before,
              rip: 0x4000n,
              registers: before.registers.map((value, i) => (i === 4 ? 0x7ff0n : value)),
            },
            'handler complete scalar',
          );
          equal(cpu.readPagingState().cr2, 0xdeadn, 'AC never fabricates CR2');
          clock(cpu);
          const repaired: State = { ...entered.state, registers: [...entered.state.registers] };
          repaired.registers[3] = 0x6000n;
          cpu.setState(repaired);
          const result = await cpu.run({
            budget: 3n,
            stop: PC + 2n,
            eventDelivery: { budget: 1n },
          });
          equal(result.retired, 3n, 'cleanup IRET and one repaired source');
          equal(result.deliveryAttempts, 0n, 'no repeated AC entry');
          equal(result.state.rip, PC + 2n, 'faulting source retried once');
          equal(result.state.registers[0], 0x1122334440302010n, 'actual repaired load');
          equal(result.state.registers[4], 0x8000n, 'IRET removes saved frame after cleanup');
          equal(result.state.flags, 0x40002n, 'retried begin consumes RF and preserves AC');
          clock(cpu, 103n);
        });
      },
    },
    {
      name: 'alignment checking NMI entry uses EXT one and benign recursive AC is bounded before repaired frame entry',
      async run() {
        const f = eventFixture(compiler);
        await using(side, f, async (cpu) => {
          const before = state({ 4: 0x8001n });
          cpu.setState(before);
          cpu.setTimeState({ tsc: 100n, tscAux: AUX });
          cpu.requestNmi();
          const first = await cpu.run({ budget: 1n, eventDelivery: { budget: 1n } });
          equal(first.reason, 'event-budget', 'entry refusal remains inspectable');
          equal(first.deliveryAttempts, 1n, 'one NMI entry attempt');
          equal(first.retired, 0n, 'entry/refusal no guest instruction');
          same(first.state.event, pending(1), 'EXT1 pending alignment source');
          equal(first.state.registers[4], 0x8001n, 'refused frame does not publish SP');
          same(
            first.state.interrupts,
            { irq: null, nmiPending: false, nmiBlocked: true },
            'NMI accepted once and blocking retained',
          );
          memory(cpu, f.bytes, 'misaligned frame publishes no payload');
          const recursive = await cpu.run({ budget: 1n, eventDelivery: { budget: 1n } });
          equal(recursive.reason, 'event-budget', 'benign AC secondary is bounded');
          same(recursive.state.event, pending(1), 'AC while delivering AC remains serial not DF');
          equal(recursive.deliveryAttempts, 1n, 'one bounded recursive attempt');
          equal(recursive.retired, 0n, 'no recursion retirement');
          const repair: State = { ...recursive.state, registers: [...recursive.state.registers] };
          repair.registers[4] = 0x8000n;
          cpu.setState(repair);
          const entered = await cpu.run({
            budget: 1n,
            stop: 0x4000n,
            eventDelivery: { budget: 1n },
          });
          equal(entered.reason, 'stop', 'repaired pending source enters');
          equal(entered.retired, 0n, 'entry is not retired');
          same(
            cpu.readPhysicalMemory(0xbff0n, 16),
            hex('01000000001000000b00000002000500'),
            'literal secondary error1 frame after separate repair',
          );
          equal(cpu.readPagingState().cr2, 0xdeadn, 'EXT AC retains CR2');
          clock(cpu);
        });
      },
    },
    {
      name: 'alignment checking pending code validation is atomic and captured events survive a disabled live gate',
      async run() {
        const f = eventFixture(compiler);
        await using(side, f, async (cpu) => {
          for (const code of [0, 1] as const) {
            const before = { ...state(), event: pending(code) };
            cpu.setState(before);
            same(cpu.readState(), before, 'valid pending AC round trip');
            for (const invalid of [2, 0x10000]) {
              let thrown = false;
              try {
                cpu.setState({
                  ...before,
                  event: {
                    ...pending(0),
                    exception: { ...pending(0).exception, errorCode: invalid },
                  },
                } as State);
              } catch {
                thrown = true;
              }
              equal(thrown, true, 'invalid pending AC code rejected');
              same(cpu.readState(), before, 'bad code publishes no state');
              memory(cpu, f.bytes, 'bad pending code no memory effects');
            }
            cpu.setPagingState({ ...cpu.readPagingState(), cr0: cpu.readPagingState().cr0 & ~AC });
            same(cpu.readState(), before, 'stored event not eligibility-derived');
          }
        });
      },
    },
    {
      name: 'alignment checking target CPL zero exempts an intentionally unaligned legacy kernel frame',
      async run() {
        const f = eventFixture(compiler),
          context = f.options.context as ExecutionContextSnapshot;
        context.tr = { selector: 0x28, valid: true, base: 0xb000n, limit: 0x67, attributes: 0x8b };
        f.bytes.set(hex(gate(32, 0x18, 0x4000n, 0)), 0xc000 + 17 * 8);
        f.bytes.set(hex('018000002000'), 0xf004);
        await using(side, f, async (cpu) => {
          const before = state({ 4: 0x9000n });
          cpu.setState(before);
          const result = await cpu.run({
            budget: 1n,
            stop: 0x4000n,
            eventDelivery: { budget: 1n },
          });
          equal(result.reason, 'stop', 'unaligned prospective kernel frame eligible by target CPL');
          equal(result.retired, 0n, 'source fault no instruction');
          equal(result.deliveryAttempts, 1n, 'one ring transition');
          equal(cpu.readContext().cpl, 0, 'actual CPL transition');
          equal(result.state.registers[4], 0x7fe9n, 'unaligned kernel SP minus six dwords');
          same(
            cpu.readPhysicalMemory(0xbfe9n, 24),
            hex('00000000001000000b000000020005000090000013000000'),
            'literal kernel frame retains source fields',
          );
          equal(result.alignmentCheck, undefined, 'source is delivered, no recursive frame AC');
        });
      },
    },
  ];
}
