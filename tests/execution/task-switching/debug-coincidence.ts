import {
  CALL,
  ENTRY_B,
  ENTRY_C,
  TSS_A,
  TSS_B,
  check,
  dword,
  equal,
  same,
  toCaller,
  withGuest,
  write32,
  type Scenario,
} from './fixture.js';

export function taskDebugCoincidenceScenario(compiler: WebAssembly.Module): Scenario {
  return {
    name: 'incoming task trap and global far-pointer read report both debug status bits in one event',
    async run() {
      for (const [taskTrap, watch] of [
        [false, true],
        [true, false],
        [true, true],
      ] as const)
        await withGuest(compiler, async (cpu) => {
          await toCaller(cpu);
          const pointer = 0x78000n;
          write32(cpu, pointer, 0);
          write32(cpu, pointer + 4n, 0x20);
          // call far [0x78000]; DR0 covers the selector halfword.
          cpu.writePhysicalMemory(CALL, Uint8Array.of(0xff, 0x1d, 0x00, 0x80, 0x07, 0x00));
          write32(cpu, TSS_B + 84n, 0x10);
          if (taskTrap) cpu.writePhysicalMemory(TSS_B + 100n, Uint8Array.of(1));
          cpu.setDebugState({
            addresses: [pointer + 4n, 0n, 0n, 0n],
            dr6: 0n,
            dr7: watch ? 0x70402n : 0x70400n,
          });
          const idt = cpu.readContext().idtr.base;
          cpu.writePhysicalMemory(idt + 8n, cpu.readPhysicalMemory(idt + 80n, 8));

          const result = await cpu.run({ budget: 1n });
          equal(result.reason, 'debug-exception', '#DB after indirect CALL');
          equal(result.retired, 1n, 'one CALL retired');
          equal(result.state.rip, ENTRY_B, 'B has not started');
          equal(cpu.readContext().tr.selector, 0x20, 'B is the active task');
          equal(dword(cpu, TSS_A + 32n), Number(CALL + 6n), 'post-CALL return EIP');
          equal(dword(cpu, TSS_B) & 0xffff, 0x18, 'B backlink identifies A');
          equal(
            cpu.readDebugState().dr6 & 0xe00fn,
            (taskTrap ? 0x8000n : 0n) | (watch ? 1n : 0n),
            'DR6 retains each enabled cause',
          );
          equal(
            result.debugException?.cause,
            taskTrap ? 'task-switch' : 'data-breakpoint',
            'cause',
          );
          equal(result.debugException?.breakpoints, watch ? 1 : undefined, 'public comparator');
          equal(result.debugException?.resumePc, ENTRY_B, 'debug handler resumes B');
          check(result.state.event.kind === 'pending', 'one staged exception');
          equal(result.state.event.exception.returnPc, ENTRY_B, 'pending return EIP');
          if (taskTrap && watch) {
            same(
              result.state.event.exception.debug,
              { class: 'trap', cause: 'task-switch', breakpoints: 1 },
              'pending event preserves BT and B0',
            );
            await withGuest(compiler, async (restored) => {
              restored.writePhysicalMemory(0n, cpu.readPhysicalMemory(0n, 0x110000));
              restored.setContext(cpu.readContext());
              restored.setPagingState(cpu.readPagingState());
              restored.setDebugState(cpu.readDebugState());
              restored.setState(result.state);
              same(restored.readState().event, result.state.event, 'portable pending debug cause');
              const delivered = await restored.run({
                budget: 1n,
                eventDelivery: { budget: 1n },
                stop: ENTRY_C,
              });
              equal(delivered.reason, 'stop', 'C handles the combined trap');
              equal(delivered.retired, 0n, 'no B or C instruction retired');
              equal(delivered.deliveryAttempts, 1n, 'one debug delivery');
              equal(delivered.state.event.kind, 'ready', 'debug event consumed');
              equal(dword(restored, TSS_B + 32n), Number(ENTRY_B), 'B saved at its first EIP');
              equal(restored.readDebugState().dr6 & 0xe00fn, 0x8001n, 'C sees both DR6 bits');
            });
          }
        });
    },
  };
}
