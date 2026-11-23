import { routingScenarios } from './delivery/routing.js';
import { finalOwnerReviewScenarios } from './delivery/task-and-restore.js';
import { reviewBoundaryScenarios } from './delivery/transfer-operands.js';
import { activityReviewScenarios } from './breakpoints/activation.js';
import { registerMatrixScenarios } from './registers/encodings.js';
import { correctnessReviewScenarios } from './registers/validation.js';
import { transitionScenarios } from './delivery/transitions.js';
import { operandScenarios } from './breakpoints/operands.js';
import { shadowScenarios } from './delivery/shadows.js';
import { dataScenarios } from './breakpoints/data.js';
import { bankScenarios } from './registers/bank.js';
import type { Scenario } from '../../../support/fixture.js';
import { registerScenarios } from './registers/movdr.js';
import { executeScenarios } from './breakpoints/execute.js';
import { recoveryScenarios } from './delivery/recovery.js';
export function hardwareDebugScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    ...bankScenarios(compiler),
    ...dataScenarios(compiler),
    ...operandScenarios(compiler),
    ...shadowScenarios(compiler),
    ...transitionScenarios(compiler),
    ...registerScenarios(compiler),
    ...registerMatrixScenarios(compiler),
    ...correctnessReviewScenarios(compiler),
    ...reviewBoundaryScenarios(compiler),
    ...activityReviewScenarios(compiler),
    ...finalOwnerReviewScenarios(compiler),
    ...routingScenarios(compiler),
    ...executeScenarios(compiler),
    ...recoveryScenarios(compiler),
  ];
}
