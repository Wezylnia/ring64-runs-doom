import {
  CODE,
  changed,
  createFixture,
  equal,
  initial,
  state,
  type Scenario,
} from '../../../support/fixture.js';
import { instructionScenarios } from './instructions.js';
import { rightsScenarios } from './rights.js';
import { cacheScenarios } from './cache.js';
import { consumerScenarios } from './consumers.js';
import { eventScenarios } from './events.js';
import { boundaryScenarios } from './boundaries.js';

export function protectionKeysScenarios(compiler: WebAssembly.Module): Scenario[] {
  const using = createFixture(compiler);
  const featureDisabled = (
    [
      ['RDPKRU', '0f01ee'],
      ['WRPKRU', '0f01ef'],
    ] as const
  ).map(([name, code]) => ({
    name: `protection keys ${name} with PKE clear reaches UD without retirement`,
    async run() {
      await using(code, async (cpu) => {
        // The private linear model has no paging controls, so PKE is clear. Low
        // ECX/EDX are deliberately invalid: the feature gate selects #UD first.
        const before = initial(CODE, { 1: 1n, 2: 1n }, 0x10202n);
        cpu.setState(before);
        cpu.setTimeState({ tsc: 100n, tscAux: 0x89abcdef });
        const report = await cpu.run({ budget: 1n });
        equal(report.reason, 'invalid-encoding', `${name} PKE-clear report`);
        equal(report.retired, 0n, `${name} PKE-clear retirement`);
        equal(report.remaining, 1n, `${name} PKE-clear remaining budget`);
        state(report.state, changed(before, CODE, {}, 0x202n));
        equal(cpu.readTimeState().tsc, 100n, `${name} PKE-clear time`);

        cpu.setState(before);
        const staged = await cpu.run({ budget: 1n, eventDelivery: { budget: 0n } });
        equal(staged.reason, 'event-budget', `${name} PKE-clear staged UD`);
        equal(staged.retired, 0n, `${name} staged retirement`);
        const expected = changed(before, CODE, {}, 0x202n);
        expected.event = {
          kind: 'pending',
          exception: {
            vector: 6,
            instructionPc: CODE,
            returnPc: CODE,
            savedFlags: 0x10202n,
          },
        };
        state(staged.state, expected);
        equal(cpu.readTimeState().tsc, 100n, `${name} staged time`);
      });
    },
  }));
  return [
    ...featureDisabled,
    ...instructionScenarios(compiler),
    ...rightsScenarios(compiler),
    ...cacheScenarios(compiler),
    ...consumerScenarios(compiler),
    ...eventScenarios(compiler),
    ...boundaryScenarios(compiler),
  ];
}
