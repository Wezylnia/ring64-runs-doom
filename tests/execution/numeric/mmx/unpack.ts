import { equal, initial, PC, usingPaged, type Scenario } from '../../memory/paging/fixture.js';
import { clone, stack, x87 } from '../x87/fixture.js';

export function unpackMemoryScenario(compiler: WebAssembly.Module): Scenario {
  return {
    name: 'MMX low unpack reads four bytes, high unpack eight and XMM unpack sixteen',
    async run() {
      const edge = PC + 0xffcn;
      for (const [low, high, expected] of [
        ['60', '68', 0xd444c333b222a111n],
        ['61', '69', 0xd4c34433b2a12211n],
        ['62', '6a', 0xd4c3b2a144332211n],
      ] as const) {
        await usingPaged(compiler, { code: `0f${low}030f${high}03660f${low}03` }, async (cpu) => {
          cpu.writePhysicalMemory(0x9ffcn, Uint8Array.of(0xa1, 0xb2, 0xc3, 0xd4));
          const before = stack({}, 5);
          before.registers[0] = 0x8877665544332211n;
          const after = clone(before);
          after.registers[0] = (0xffffn << 64n) | expected;
          after.statusWord &= ~0x3800;
          after.tagWord = 0;
          cpu.setX87State(before);
          cpu.setState(initial(PC, { 3: edge }));
          const lowRead = await cpu.run({ budget: 1n });
          equal(lowRead.reason, 'budget', 'm32 fits at page end');
          equal(lowRead.retired, 1n, 'low unpack retired');
          x87(cpu, after);

          cpu.setX87State(before);
          cpu.setState(initial(PC + 3n, { 3: edge }));
          const highRead = await cpu.run({ budget: 1n });
          equal(highRead.reason, 'page-fault', 'm64 crosses page end');
          equal(highRead.pageFault?.address, edge + 4n, 'first absent high unpack byte');
          equal(highRead.retired, 0n, 'high unpack restartable');
          x87(cpu, before);

          // Watch the last byte and the first byte beyond each operand, both in mapped RAM.
          for (const [offset, width] of [
            [0, 4],
            [3, 8],
            [6, 16],
          ] as const) {
            const address = PC + 0x300n;
            cpu.setState(initial(PC + BigInt(offset), { 3: address }));
            cpu.setDebugState({
              addresses: [address + BigInt(width - 1), address + BigInt(width), 0n, 0n],
              dr6: 0n,
              dr7: 0x330405n,
            });
            const watched = await cpu.run({ budget: 1n });
            equal(watched.reason, 'debug-exception', `m${width * 8} last byte watched`);
            equal(watched.retired, 1n, 'watchpoint after retirement');
            equal(cpu.readDebugState().dr6 & 3n, 1n, 'first byte beyond operand excluded');
          }
        });
      }
    },
  };
}
