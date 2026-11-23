import {
  AFTER_EXIT,
  ENTRY_B,
  TSS_B,
  check,
  dword,
  equal,
  markers,
  same,
  toCaller,
  withGuest,
  write32,
  type Scenario,
} from './fixture.js';

export function invalidTaskSelectorScenario(compiler: WebAssembly.Module): Scenario {
  return {
    name: 'unusable incoming CS or LDTR reports a late task fault and survives real nested repair',
    async run() {
      for (const [field, repaired] of [
        [76, 8],
        [96, 0],
      ] as const)
        await withGuest(compiler, async (cpu, ports) => {
          await toCaller(cpu);
          write32(cpu, TSS_B + 84n, 0x10); // B.DS valid; fault now comes from a different field.
          write32(cpu, TSS_B + BigInt(field), 0x38);
          // Repoint only C's existing "mov word [B.DS], DATA" at the selected TSS field.
          equal(dword(cpu, 0x10035fn), Number(TSS_B + 84n), 'original repair operand');
          write32(cpu, 0x10035fn, Number(TSS_B) + field);
          cpu.writePhysicalMemory(0x100363n, Uint8Array.of(repaired, 0));
          const fault = await cpu.run({ budget: 2n, eventDelivery: { budget: 0n } });
          equal(fault.reason, 'event-budget', 'late fault parked before delivery');
          equal(fault.retired, 1n, 'outgoing CALL charged once');
          equal(fault.state.rip, ENTRY_B, 'incoming B owns late fault');
          check(fault.state.event.kind === 'pending', 'staged selector fault');
          equal(fault.state.event.exception.vector, 10, 'invalid task selector faults #TS');
          equal(fault.state.event.exception.errorCode, 0x38, 'offending selector');
          equal(fault.state.event.exception.returnPc, ENTRY_B, 'incoming exception return PC');
          const ctx = cpu.readContext();
          equal(ctx.tr.selector, 0x20, 'current task B');
          equal(ctx.segments.cs.usable, false, 'new CS not qualified');
          equal(ctx.segments.ss.usable, false, 'new SS not qualified');
          if (field === 76) equal(ctx.segments.cs.selector, 0x38, 'invalid CS visible');
          else {
            equal(ctx.ldtr.selector, 0x38, 'invalid LDTR visible');
            equal(ctx.ldtr.valid, false, 'LDTR cache not loaded');
          }
          cpu.setContext(ctx);
          cpu.setState(fault.state);
          same(cpu.readState().event, fault.state.event, 'same-CPU pending checkpoint');
          const resumed = await cpu.run({
            budget: 1000n,
            eventDelivery: { budget: 1n },
            stop: AFTER_EXIT,
          });
          equal(resumed.reason, 'stop', 'C repairs incoming selector and both IRETs complete');
          equal(resumed.deliveryAttempts, 1n, 'one task-gate delivery');
          same(ports, [...markers('SCRBAP'), [0xf4, 32, 1]], 'all repaired guest port writes');
        });
    },
  };
}
