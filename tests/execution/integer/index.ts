import { registerScenarios } from './registers.js';
import { integerConditionsScenarios } from './conditions/index.js';
import { integerMemoryScenarios } from './memory/index.js';
import { scalarDataScenarios } from './data/index.js';
import { integerArithmeticScenarios } from './arithmetic/index.js';
import { multiplyAndDivideScenarios } from './multiply/index.js';
import { compilerBitmapScenarios } from './compiler-bitmap.js';
import { nopScenarios } from './nop.js';

export function integerScenarios(core: WebAssembly.Module) {
  return [
    ...registerScenarios(core),
    ...integerConditionsScenarios(core),
    ...integerMemoryScenarios(core),
    ...scalarDataScenarios(core),
    ...integerArithmeticScenarios(core),
    ...multiplyAndDivideScenarios(core),
    ...compilerBitmapScenarios(core),
    ...nopScenarios(core),
  ];
}
