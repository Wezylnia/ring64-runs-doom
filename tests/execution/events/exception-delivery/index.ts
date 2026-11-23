import type { Scenario } from '../../../support/fixture.js';
import { basicScenarios } from './frames/basic.js';
import { boundaryScenarios } from './dispatch/boundaries.js';
import { numericScenarios } from './frames/numeric.js';
import { escalationScenarios } from './dispatch/escalation.js';
import { modeScenarios } from './frames/modes.js';
import { memoryScenarios } from './frames/memory.js';
import { stateScenarios } from './state.js';
import { frontierScenarios } from './dispatch/frontiers.js';
import { lifecycleScenarios } from './lifecycle.js';

export function exceptionDeliveryScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    ...basicScenarios(compiler),
    ...boundaryScenarios(compiler),
    ...numericScenarios(compiler),
    ...escalationScenarios(compiler),
    ...modeScenarios(compiler),
    ...memoryScenarios(compiler),
    ...stateScenarios(compiler),
    ...frontierScenarios(compiler),
    ...lifecycleScenarios(compiler),
  ];
}
