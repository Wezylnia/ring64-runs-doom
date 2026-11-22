import { PC, equal, initial, little, usingPaged, type Scenario } from './fixture.js';

export function pagedRegionScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'paged dynamic regions learn returning targets and recheck edited code',
      async run() {
        const data = PC + 0x1000n;
        const stack = PC + 0x2ff0n;
        const left = PC + 0x40n;
        const right = PC + 0x50n;
        const stop = PC + 14n;
        // MOV ECX,640; loop: MOV RAX,[RSI]; CALL RAX; DEC ECX; JNZ loop; NOP.
        // Two independent ADD EDI,imm8; RET handlers on the same code page.
        const code =
          'b980020000488b06ffd0ffc975f790'.padEnd(0x80, '82') +
          '83c701c3'.padEnd(0x20, '82') +
          '83c702c3';
        await usingPaged(
          compiler,
          {
            code,
            words: [
              [0x4008n, 0xa007n],
              [0x4010n, 0xb007n],
            ],
            spans: [
              { physical: 0xa000n, bytes: little([left]) },
              { physical: 0xb000n, bytes: new Uint8Array(4096) },
            ],
          },
          async (cpu) => {
            const starting = initial(PC, { 4: stack, 6: data, 7: 0n }, 2n);
            const execute = async (value: bigint) => {
              cpu.setState(starting);
              const result = await cpu.run({ stop, budget: 4000n });
              equal(result.reason, 'stop', 'the guest reaches its own stop');
              equal(result.retired, 3841n, 'all CALL and RET instructions retire once');
              equal(result.state.registers[4], stack, 'stack returns to its starting address');
              equal(result.state.registers[7], value, 'the observed target actually executed');
              return result.resources.dispatches;
            };
            const first = await execute(640n);
            if (first > 520)
              throw new Error('the learned run exceeded its dispatch training window');
            const warmed = await execute(640n);
            if (warmed > 2) throw new Error('the selected hot region still exits on return');
            cpu.writePhysicalMemory(0xa000n, little([right]));
            await execute(1280n);
            cpu.writePhysicalMemory(0xa000n, little([left]));
            cpu.writePhysicalMemory(0x9042n, Uint8Array.of(3));
            await execute(1920n);

            cpu.setState({ ...starting, flags: 0x102n });
            const stepped = await cpu.run({ stop, budget: 4000n });
            equal(stepped.reason, 'debug-exception', 'single-step still takes the checked exit');
            equal(stepped.retired, 1n, 'single-step sees only the setup MOV');
            equal(stepped.state.rip, PC + 5n, 'the guest can resume at the loop head');
          },
        );
      },
    },
  ];
}
