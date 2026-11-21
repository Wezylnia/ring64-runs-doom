import { operandScenarios } from './operands.js';
import { accessScenarios } from './access.js';
import { decodeScenarios } from './decode.js';
import { limitScenarios } from './limits.js';
import { observationScenarios } from './observation.js';
import { routingScenarios } from './routing.js';
import { compoundStackScenarios } from './compound/index.js';

export function stackScenarios(core: WebAssembly.Module) {
  return [
    ...operandScenarios(core),
    ...accessScenarios(core),
    ...decodeScenarios(core),
    ...limitScenarios(core),
    ...observationScenarios(core),
    ...routingScenarios(core),
    ...compoundStackScenarios(core),
  ];
}
