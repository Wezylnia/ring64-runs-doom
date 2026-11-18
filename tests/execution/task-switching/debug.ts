import {
  ENTRY_B,
  TSS_B,
  equal,
  markers,
  same,
  toCaller,
  withGuest,
  write32,
  type Scenario,
} from './fixture.js';

export function taskDebugScenario(compiler: WebAssembly.Module): Scenario {
  return {
    name: 'task T requests DR6.BT before incoming execution and does not inherit outgoing single-step',
    async run() {
      for (const entry of ['plain', 'trap', 'pending'] as const)
        await withGuest(compiler, async (cpu, ports) => {
          await toCaller(cpu);
          write32(cpu, TSS_B + 84n, 0x10);
          if (entry !== 'plain') cpu.writePhysicalMemory(TSS_B + 100n, Uint8Array.of(1));
          const state = cpu.readState();
          cpu.setState({ ...state, flags: state.flags | 0x100n });
          // Local DR0 and global DR1 are both armed, away from this guest's source or target code.
          cpu.setDebugState({ addresses: [0x78000n, 0x79000n, 0n, 0n], dr6: 0n, dr7: 0x409n });
          const result = await cpu.run({
            budget: entry === 'plain' ? 1n : 2n,
            ...(entry === 'pending' ? { eventDelivery: { budget: 0n } } : {}),
          });
          equal(result.retired, 1n, 'only CALL retired');
          equal(result.state.rip, ENTRY_B, 'B has not retired its first instruction');
          equal(result.state.registers[0], 0x55667788n, 'untouched incoming EAX');
          equal(cpu.readDebugState().dr7 & 255n, 8n, 'global DR1 survives; local DR0 cleared');
          equal(
            cpu.readDebugState().dr6 & 0xe00fn,
            entry === 'plain' ? 0n : 0x8000n,
            'BT is the sole selected debug cause',
          );
          if (entry === 'plain') {
            equal(result.reason, 'budget', 'source TF alone does not single-step the task switch');
            equal(result.debugException, undefined, 'no outgoing debug trap');
          } else if (entry === 'trap') {
            equal(result.reason, 'debug-exception', 'T flag reports #DB');
            equal(result.debugException?.cause, 'task-switch', 'public BT cause');
            equal(result.debugException?.class, 'trap', 'trap class');
            equal(result.debugException?.instructionPc, ENTRY_B, 'incoming EIP');
            equal(result.debugException?.resumePc, ENTRY_B, 'resume before first instruction');
            equal(result.debugException?.resumeFlags, 0x4002n, 'exact incoming nested EFLAGS');
            equal(result.debugException?.instructionCompleted, false, 'B instruction not executed');
          } else {
            equal(result.reason, 'event-budget', 'task trap retained for delivery');
            if (result.state.event.kind !== 'pending')
              throw Error('expected pending task-switch #DB');
            equal(result.state.event.exception.vector, 1, 'pending #DB');
            equal(result.state.event.exception.savedFlags, 0x4002n, 'pending incoming EFLAGS');
            same(
              result.state.event.exception.debug,
              { class: 'trap', cause: 'task-switch' },
              'checkpoint preserves BT without breakpoint slots',
            );
            await withGuest(compiler, async (restored) => {
              restored.writePhysicalMemory(0n, cpu.readPhysicalMemory(0n, 0x110000));
              restored.setContext(cpu.readContext());
              restored.setPagingState(cpu.readPagingState());
              restored.setDebugState(cpu.readDebugState());
              restored.setState(result.state);
              const saved = await restored.run({ budget: 1n, eventDelivery: { budget: 0n } });
              equal(saved.reason, 'event-budget', 'separate CPU retains pending BT');
              equal(saved.retired, 0n, 'no B instruction precedes pending trap');
              same(saved.state.event, result.state.event, 'portable public checkpoint');
            });
          }
          same(ports, markers('S'), 'incoming instruction stream silent');
        });
    },
  };
}
