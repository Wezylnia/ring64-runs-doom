import {
  CALL,
  ENTRY_C,
  TSS_B,
  TSS_C,
  equal,
  toCaller,
  withGuest,
  write32,
  type Scenario,
} from '../fixture.js';

export function taskDebugEntryScenario(compiler: WebAssembly.Module): Scenario {
  return {
    name: 'task-gate #DB and INT1 clear DR7.GD only after successful handler entry',
    async run() {
      for (const software of [false, true]) {
        for (const badDs of [false, true]) {
          await withGuest(compiler, async (cpu) => {
            await toCaller(cpu);
            const idt = cpu.readContext().idtr.base;
            cpu.writePhysicalMemory(idt + 8n, cpu.readPhysicalMemory(idt + 80n, 8));
            if (badDs) write32(cpu, TSS_C + 84n, 0x38);
            cpu.writePhysicalMemory(ENTRY_C, Uint8Array.of(0x0f, 0x21, 0xc0)); // MOV EAX,DR0
            if (software) {
              cpu.writePhysicalMemory(CALL, Uint8Array.of(0xf1)); // INT1
            } else {
              write32(cpu, TSS_B + 84n, 0x10);
              cpu.writePhysicalMemory(TSS_B + 100n, Uint8Array.of(1));
            }
            const debug = cpu.readDebugState();
            cpu.setDebugState({ ...debug, dr7: debug.dr7 | 0x2000n });
            if (!software) {
              const trap = await cpu.run({ budget: 1n });
              equal(trap.reason, 'debug-exception', 'incoming T flag triggered #DB');
              equal(
                cpu.readDebugState().dr7 & 0x2000n,
                0x2000n,
                'selection precedes handler entry',
              );
            }
            const entry = await cpu.run({
              budget: 2n,
              stop: ENTRY_C,
              ...(software ? {} : { eventDelivery: { budget: 1n } }),
            });
            if (badDs) {
              equal(
                entry.reason,
                software ? 'protection-fault' : 'event-budget',
                'incoming #TS precedes C code',
              );
              equal(cpu.readDebugState().dr7 & 0x2000n, 0x2000n, 'failed #DB entry retained GD');
            } else {
              equal(entry.reason, 'stop', 'arrived at #DB handler');
              equal(cpu.readDebugState().dr7 & 0x2000n, 0n, 'successful #DB entry cleared GD');
              const read = await cpu.run({ budget: 1n });
              equal(read.reason, 'budget', 'MOV DR did not recursively trap');
              equal(read.state.rip, ENTRY_C + 3n, 'handler MOV DR retired');
            }
          });
        }
      }
    },
  };
}
