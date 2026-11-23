import { matrixScenarios } from './operands/matrix.js';
import { stackMatrixScenarios } from './operands/stack-matrix.js';
import { classScenarios } from './operands/classes.js';
import { boundaryScenarios } from './priority/boundaries.js';
import { orderedScenarios } from './priority/ordered.js';
import { eventScenarios } from './priority/events.js';
import { lifecycleScenarios } from './priority/lifecycle.js';
import { largePageScenarios } from './priority/large-pages.js';
import { positiveScenarios } from './operands/positive.js';
import { operandScenarios } from './operands/residues.js';
import { priorityScenarios } from './priority/faults.js';
import { modelScenarios } from './operands/models.js';

export function alignmentCheckingScenarios(compiler: WebAssembly.Module) {
  return [
    ...matrixScenarios(compiler),
    ...stackMatrixScenarios(compiler),
    ...classScenarios(compiler),
    ...boundaryScenarios(compiler),
    ...orderedScenarios(compiler),
    ...eventScenarios(compiler),
    ...lifecycleScenarios(compiler),
    ...largePageScenarios(compiler),
    ...positiveScenarios(compiler),
    ...operandScenarios(compiler),
    ...priorityScenarios(compiler),
    ...modelScenarios(compiler),
  ];
}
