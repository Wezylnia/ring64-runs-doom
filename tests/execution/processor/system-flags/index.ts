import { stateScenarios } from './state/capture.js';
import { transferScenarios } from './state/transfers.js';
import { privilegeScenarios } from './state/privileges.js';
import { steppingScenarios } from './stepping/single-step.js';
import { repeatedScenarios } from './stepping/repeated.js';
import { shadowScenarios } from './stepping/shadows.js';
import { aliasScenarios } from './state/aliases.js';
import { preservationScenarios } from './state/preservation.js';
import { memoryScenarios } from './state/memory.js';
import { numericBoundaryScenarios } from './stepping/numeric-boundaries.js';
import { cacheScenarios } from './state/cache.js';
import { acceptanceBoundaryScenarios } from './stepping/acceptance-boundaries.js';
import { reviewScenarios } from './stepping/shadow-history.js';
import { numericCompletionScenarios } from './stepping/numeric-completion.js';
import type { Scenario } from './fixture.js';

export function systemFlagScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    ...stateScenarios(compiler),
    ...transferScenarios(compiler),
    ...privilegeScenarios(compiler),
    ...steppingScenarios(compiler),
    ...repeatedScenarios(compiler),
    ...shadowScenarios(compiler),
    ...aliasScenarios(compiler),
    ...preservationScenarios(compiler),
    ...memoryScenarios(compiler),
    ...numericBoundaryScenarios(compiler),
    ...cacheScenarios(compiler),
    ...acceptanceBoundaryScenarios(compiler),
    ...reviewScenarios(compiler),
    ...numericCompletionScenarios(compiler),
  ];
}
