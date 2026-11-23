import {
  CALL,
  IRET_C,
  TSS_A,
  TSS_C,
  dword,
  equal,
  markers,
  same,
  toCaller,
  withGuest,
  write32,
  type Scenario,
} from '../fixture.js';

export function deferredTaskDebugScenario(compiler: WebAssembly.Module): Scenario {
  return {
    name: 'a watched MOV SS followed by a task-gate INT delivers its data breakpoint in the new task',
    async run() {
      await withGuest(compiler, async (cpu, ports) => {
        await toCaller(cpu);
        const watched = 0x78000n;
        write32(cpu, watched, 0x10);
        write32(cpu, TSS_C + 32n, Number(IRET_C));
        // mov ss,word [0x78000]; int 10 (IDT task gate to C)
        cpu.writePhysicalMemory(CALL, Uint8Array.of(0x8e, 0x15, 0x00, 0x80, 0x07, 0x00, 0xcd, 10));
        cpu.setDebugState({ addresses: [watched, 0n, 0n, 0n], dr6: 0n, dr7: 0x70401n });

        const producer = await cpu.run({ budget: 1n });
        equal(producer.reason, 'budget', 'MOV SS stops before INT');
        equal(producer.state.rip, CALL + 6n, 'INT is next');
        equal(producer.state.interruptibility.ss, true, 'SS inhibition active');
        same(
          producer.state.debugContinuation,
          { breakpoints: 1, originPc: CALL },
          'the explicit selector read matched local DR0',
        );
        const entry = await cpu.run({ budget: 1n });
        equal(entry.reason, 'debug-exception', 'historical breakpoint delivered after entering C');
        equal(entry.retired, 1n, 'software INT retired');
        equal(entry.state.rip, IRET_C, 'no handler instruction executed');
        equal(cpu.readContext().tr.selector, 0x28, 'C active');
        equal(entry.debugException?.instructionPc, CALL + 6n, 'completed INT');
        equal(entry.debugException?.resumePc, IRET_C, 'first incoming instruction');
        equal(entry.debugException?.instructionCompleted, true, 'INT completed before #DB');
        equal(entry.debugException?.breakpoints, 1, 'retained data comparator');
        equal(entry.debugException?.deferredOriginPc, CALL, 'earlier MOV SS caused the hit');
        equal(entry.state.debugContinuation, undefined, 'deferred hit consumed');
        equal(cpu.readDebugState().dr7 & 0x55n, 0n, 'local enables cleared by the switch');
        equal(cpu.readDebugState().dr6 & 0xe00fn, 1n, 'B0 reported after DR7.L0 cleared');
        equal(dword(cpu, TSS_A + 32n), Number(CALL + 8n), 'outgoing TSS saved post-INT EIP');
        same(ports, markers('S'), 'C has not run');
      });
    },
  };
}
