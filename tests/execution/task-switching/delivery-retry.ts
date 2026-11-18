import {
  ENTRY_C,
  TSS_B,
  TSS_C,
  dword,
  equal,
  markers,
  same,
  toCaller,
  withGuest,
  write32,
  type Scenario,
} from './fixture.js';

export function taskDeliveryRetryScenario(compiler: WebAssembly.Module): Scenario {
  return {
    name: 'repairing the committed task gate error stack completes the original delivery once',
    async run() {
      await withGuest(compiler, async (cpu, ports) => {
        await toCaller(cpu);
        const slot = 0x9bffc;
        const span = (offset: number, length: number) => ({
          base: BigInt(offset),
          backing: 'ram',
          offset,
          length,
          permissions: { read: true, write: true, execute: true },
        });
        cpu.setMappings([span(0, slot), span(slot + 4, 0x110000 - slot - 4)]);
        const clock = cpu.readTimeState().tsc;
        const first = await cpu.run({ budget: 1n, eventDelivery: { budget: 1n } });
        equal(first.reason, 'physical-memory-fault', 'C error slot unavailable');
        equal(first.retired, 1n, 'outgoing CALL accounted');
        equal(first.deliveryAttempts, 1n, 'original #TS acceptance');
        equal(first.state.rip, ENTRY_C, 'C authoritative');
        equal(first.state.registers[4], BigInt(slot + 4), 'failed push leaves C.ESP');
        equal(first.state.taskSwitchIncomplete, true, 'incomplete delivery snapshot');
        equal(cpu.readContext().tr.selector, 0x28, 'C already selected');

        write32(cpu, TSS_B + 32n, 0xb1b2b3b4);
        write32(cpu, TSS_C, 0xc1c2c3c4);
        const stalled = await cpu.run({ budget: 1n, eventDelivery: { budget: 0n } });
        equal(stalled.reason, 'physical-memory-fault', 'original missing slot still selected');
        equal(stalled.retired, 0n, 'qualification executes no instruction');
        equal(stalled.deliveryAttempts, 0n, 'no second task-gate delivery');
        equal(cpu.readTimeState().tsc, clock + 1n, 'private retry costs no guest ticks');
        cpu.setMappings([span(0, 0x110000)]);
        const done = await cpu.run({ budget: 1n, eventDelivery: { budget: 0n } });
        equal(done.reason, 'budget', 'first C instruction completed');
        equal(done.retired, 1n, 'only the incoming instruction retired');
        equal(done.deliveryAttempts, 0n, 'repair uses original allowance');
        equal(done.state.taskSwitchIncomplete, undefined, 'completed snapshot is portable');
        equal(done.state.registers[4], BigInt(slot), 'one error push');
        equal(dword(cpu, BigInt(slot)), 0x38, 'original selector error code');
        equal(dword(cpu, TSS_B + 32n), 0xb1b2b3b4, 'B save not replayed');
        equal(dword(cpu, TSS_C), 0xc1c2c3c4, 'C backlink not rewritten');
        same(ports, markers('S'), 'repair did not repeat a port write');
      });
    },
  };
}
