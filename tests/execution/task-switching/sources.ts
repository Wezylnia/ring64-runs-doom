import {
  CALL,
  IRET_C,
  PD_A,
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
} from './fixture.js';

export function gateSourceScenario(compiler: WebAssembly.Module): Scenario {
  return {
    name: '32-bit IDT task gates preserve INT, IRQ and NMI continuation and stack conventions',
    async run() {
      for (const source of ['int', 'irq', 'nmi'] as const)
        await withGuest(compiler, async (cpu, ports) => {
          await toCaller(cpu);
          const state = cpu.readState();
          cpu.setState({ ...state, flags: state.flags | 0x200n });
          // Vector 10 already names C. Install the same gate at vector 2 for NMI.
          const idt = cpu.readContext().idtr.base;
          cpu.writePhysicalMemory(idt + 2n * 8n, cpu.readPhysicalMemory(idt + 10n * 8n, 8));
          write32(cpu, TSS_C + 32n, Number(IRET_C));
          if (source === 'int') cpu.writePhysicalMemory(CALL, Uint8Array.of(0xcd, 10));
          else if (source === 'irq') equal(cpu.requestInterrupt(10), true, 'IRQ accepted');
          else cpu.requestNmi();

          const entry = await cpu.run({ budget: 2n, stop: IRET_C, eventDelivery: { budget: 1n } });
          equal(entry.reason, 'stop', 'gate entered without executing C');
          equal(entry.state.rip, IRET_C, 'C.EIP');
          equal(entry.retired, source === 'int' ? 1n : 0n, 'guest instruction count');
          equal(entry.deliveryAttempts, source === 'int' ? 0n : 1n, 'external delivery count');
          equal(entry.state.registers[4], 0x9c000n, 'no error word on INT or external entry');
          equal(entry.state.flags & 0x4000n, 0x4000n, 'C nested');
          equal(entry.state.interrupts.nmiBlocked, source === 'nmi', 'NMI block on delivery');
          equal(cpu.readContext().tr.selector, 0x28, 'IDT task gate selected C');
          equal(cpu.readPagingState().cr3, PD_A, 'unchanged paging root');
          equal(dword(cpu, TSS_C) & 0xffff, 0x18, 'C backlink');
          const continuation = source === 'int' ? CALL + 2n : CALL;
          equal(dword(cpu, TSS_A + 32n), Number(continuation), 'source-specific outgoing EIP');
          const returned = await cpu.run({ budget: 1n });
          equal(returned.reason, 'budget', 'one nested IRET');
          equal(returned.state.rip, continuation, 'returned exactly to interrupted boundary');
          equal(cpu.readContext().tr.selector, 0x18, 'A restored');
          same(
            returned.state.interrupts,
            { irq: null, nmiPending: false, nmiBlocked: false },
            'source consumed once, NMI unblocked by IRET',
          );
          same(ports, markers('S'), 'no handler instruction before IRET');
        });
    },
  };
}
