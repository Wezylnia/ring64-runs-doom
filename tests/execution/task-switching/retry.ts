import {
  CALL,
  ENTRY_B,
  GDT,
  PD_B,
  TSS_A,
  TSS_B,
  dword,
  equal,
  markers,
  same,
  throws,
  toCaller,
  withGuest,
  write32,
  type Scenario,
} from './fixture.js';

export function taskRetryScenario(compiler: WebAssembly.Module, jump = false): Scenario {
  const transfer = jump ? 'JMP' : 'CALL';
  return {
    name: `committed task qualification retries a missing physical descriptor without replaying or charging ${transfer}`,
    async run() {
      await withGuest(compiler, async (cpu, ports) => {
        await toCaller(cpu);
        if (jump) cpu.writePhysicalMemory(CALL, Uint8Array.of(0xea));
        write32(cpu, TSS_B, 0xbadc7117);
        write32(cpu, TSS_B + 84n, 0x10);
        // B maps the code/GDT page differently. A can read both TSSes and publish its save.
        const shadow = 0x30000n;
        cpu.writePhysicalMemory(shadow, cpu.readPhysicalMemory(0x100000n, 4096));
        cpu.writePhysicalMemory(0x29000n, cpu.readPhysicalMemory(0x26000n, 4096));
        write32(cpu, 0x29000n + 0x100n * 4n, Number(shadow) | 3);
        write32(cpu, PD_B, 0x29003);
        const hole = Number(shadow + (GDT & 0xfffn) + 8n);
        const mapping = (offset: number, length: number) => ({
          base: BigInt(offset),
          backing: 'ram',
          offset,
          length,
          permissions: { read: true, write: true, execute: true },
        });
        cpu.setMappings([mapping(0, hole), mapping(hole + 8, 0x110000 - hole - 8)]);
        const clock = cpu.readTimeState().tsc;
        for (const first of [true, false]) {
          const result = await cpu.run({ budget: 2n, repeatBudget: 9n });
          equal(result.reason, 'physical-memory-fault', 'private failure resolving incoming CS');
          equal(result.retired, first ? 1n : 0n, 'only initial transfer retires');
          equal(result.state.rip, ENTRY_B, 'incoming EIP authoritative');
          equal(result.state.taskSwitchIncomplete, true, 'incomplete handoff clearly marked');
          if (first)
            await withGuest(compiler, async (other) => {
              throws(() => other.setState(result.state), 'incomplete snapshot on another CPU');
            });
          equal(cpu.readContext().tr.selector, 0x20, 'B selected before missing backing');
          equal(cpu.readPagingState().cr3, PD_B, 'lookup uses new root');
          equal(cpu.readTimeState().tsc, clock + 1n, 'no charge for failed retry');
          equal(cpu.readPhysicalMemory(GDT + 0x1dn, 1)[0], jump ? 0x89 : 0x8b, 'outgoing busy bit');
          equal(cpu.readPhysicalMemory(GDT + 0x25n, 1)[0], 0x8b, 'incoming busy bit');
          equal(
            cpu.readPhysicalMemory(shadow + (GDT & 0xfffn) + 0x25n, 1)[0],
            0x89,
            'busy writes used original root',
          );
          equal(dword(cpu, TSS_B), jump ? 0xbadc7117 : 0xbadc0018, 'backlink after publication');
        }
        // A replay of the outgoing save would erase the sentinel or reload a different B.EIP.
        cpu.writePhysicalMemory(CALL, Uint8Array.of(0x0f, 0x0b)); // poison the original far transfer
        write32(cpu, TSS_A + 32n, 0xa1b2c3d4);
        write32(cpu, TSS_B + 32n, 0x7bad00);
        cpu.setMappings([mapping(0, 0x110000)]);
        const resumed = await cpu.run({ budget: 1n, repeatBudget: 9n });
        equal(resumed.reason, 'budget', 'qualification completed and B executed');
        equal(resumed.state.taskSwitchIncomplete, undefined, 'completed snapshot portable again');
        equal(resumed.retired, 1n, 'first B instruction alone charged');
        equal(resumed.state.rip, ENTRY_B + 5n, 'CMP EAX,imm32 in the shadow page completed');
        equal(resumed.state.registers[0], 0x55667788n, 'original incoming register image');
        equal(dword(cpu, TSS_A + 32n), 0xa1b2c3d4, 'saved outgoing image not overwritten');
        equal(cpu.readTimeState().tsc, clock + 2n, 'transfer plus first B instruction');
        same(ports, markers('S'), 'no outgoing execution replayed');
      });
    },
  };
}
