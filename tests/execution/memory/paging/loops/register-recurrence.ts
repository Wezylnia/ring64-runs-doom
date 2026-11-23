import { PC, equal, initial, usingPaged, type Cpu, type Scenario } from '../fixture.js';
import { context, using } from '../../../modes/fixture.js';

export function pagedRegisterRecurrenceScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'paged affine register loops preserve masked widths, exact boundaries and retired time',
      async run() {
        // MOV R8D,EAX; SHL R8D,9; XOR EAX,R8D; XOR EAX,5F4321h;
        // ADD ECX,-3; JNZ loop; NOP.
        const code = '4189c041c1e0094431c03521435f0083c1fd75ec90';
        const seed = 0xd3bf62c5;
        const start = initial(
          PC,
          { 0: 0xcdef0000_00000000n + BigInt(seed), 1: 900n, 8: 0xffff_ffff_ffff_ffffn },
          2n,
        );
        const reference = (laps: number) => {
          let input = seed;
          let temporary = 0;
          for (let i = 0; i < laps; i++) {
            temporary = (input << 9) >>> 0;
            input = (input ^ temporary ^ 0x5f4321) >>> 0;
          }
          return { input, temporary };
        };
        for (const maxArtifactBytes of [262144, 32768]) {
          await usingPaged(compiler, { code, limits: { maxArtifactBytes } }, async (cpu) => {
            const stop = PC + 20n;
            cpu.setState(start);
            const time = cpu.readTimeState().tsc;
            const exact = await cpu.run({ stop, budget: 2000n });
            const wanted = reference(300);
            equal(exact.reason, 'stop', 'guest stop');
            equal(exact.retired, 1800n, '300 complete laps');
            equal(exact.state.registers[0], BigInt(wanted.input), 'affine result zero-extends');
            equal(
              exact.state.registers[8],
              BigInt(wanted.temporary),
              'the final scratch value is visible',
            );
            equal(exact.state.registers[1], 0n, 'count exhausted');
            equal(exact.state.flags, 0x57n, 'final counter flags are architectural');
            equal(cpu.readTimeState().tsc - time, exact.retired, 'guest clock counts skipped work');

            cpu.setState(start);
            const partial = await cpu.run({ stop, budget: 1539n });
            const prefix = reference(256).input;
            equal(partial.reason, 'budget', 'budget exits inside a lap');
            equal(partial.state.rip, PC + 10n, 'next PC is the immediate XOR');
            equal(partial.state.registers[1], 132n, 'the next decrement is pending');
            equal(
              partial.state.registers[0],
              BigInt((prefix ^ (prefix << 9)) >>> 0),
              'the partial lap is visible',
            );

            cpu.setState(start);
            const inside = await cpu.run({ stop: PC + 10n, budget: 2000n });
            equal(inside.retired, 3n, 'interior stop precedes the rest of the first lap');
            equal(
              inside.state.registers[0],
              BigInt((seed ^ (seed << 9)) >>> 0),
              'no skipped lap is published',
            );

            cpu.setState({ ...start, flags: 0x102n });
            const stepped = await cpu.run({ stop, budget: 2000n });
            equal(stepped.reason, 'debug-exception', 'TF retains the checked path');
            equal(stepped.state.rip, PC + 3n, 'single-step observes the first MOV');
            equal(stepped.retired, 1n, 'exact single-step retirement');
          });
        }
      },
    },
    {
      name: '32-bit affine register loops preserve the parked upper dwords',
      async run() {
        // MOV EDX,EAX; SHL EDX,9; XOR EAX,EDX; XOR EAX,5F4321h;
        // ADD ECX,-3; JNZ loop; NOP.
        const code = '89c2c1e20931d03521435f0083c1fd75ef90';
        const seed = 0xd3bf62c5;
        const upper = 0x11223344_00000000n;
        const check = async (cpu: Cpu) => {
          cpu.setState(initial(PC, { 0: upper + BigInt(seed), 1: upper + 900n, 2: upper }, 2n));
          const result = await cpu.run({ stop: PC + 17n, budget: 2000n });
          let x = seed;
          let shifted = 0;
          for (let i = 0; i < 300; i++) {
            shifted = (x << 9) >>> 0;
            x = (x ^ shifted ^ 0x5f4321) >>> 0;
          }
          equal(result.reason, 'stop', 'the loop reaches the next instruction');
          equal(result.retired, 1800n, 'the loop retires once per guest instruction');
          equal(result.state.registers[0], upper | BigInt(x), 'EAX update keeps the upper dword');
          equal(result.state.registers[1], upper, 'ECX update keeps the upper dword');
          equal(result.state.registers[2], upper | BigInt(shifted), 'EDX scratch is exact');
          equal(result.state.flags, 0x57n, 'ADD supplies final flags');
        };
        await using(compiler, code, context('protected'), check, { entry: PC });
        await usingPaged(compiler, { code, context: context('compatibility') }, check);
      },
    },
  ];
}
