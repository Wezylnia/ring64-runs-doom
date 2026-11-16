import { matrixScenarios } from './matrix.js';
import { classScenarios } from './classes.js';
import { boundaryScenarios } from './boundaries.js';
import { orderedScenarios } from './ordered.js';
import { eventScenarios } from './events.js';
import { lifecycleScenarios } from './lifecycle.js';
import { largePageScenarios } from './large-pages.js';
import { positiveScenarios } from './positive.js';
import { operandScenarios } from './operands.js';
import { priorityScenarios } from './priority.js';

export function alignmentCheckingScenarios(compiler: WebAssembly.Module) {
  return [
    ...matrixScenarios(compiler),
    ...classScenarios(compiler),
    ...boundaryScenarios(compiler),
    ...orderedScenarios(compiler),
    ...eventScenarios(compiler),
    ...lifecycleScenarios(compiler),
    ...largePageScenarios(compiler),
    ...positiveScenarios(compiler),
    ...operandScenarios(compiler),
    ...priorityScenarios(compiler),
  ];
}
