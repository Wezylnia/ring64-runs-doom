import type { Scenario } from '../../../support/fixture.js';
import { taskScenarios } from './task.js';
import { realScenarios } from './modes/real.js';
import { protectedScenarios } from './modes/protected.js';
import { ia32eScenarios } from './modes/ia32e.js';
import { vmScenarios } from './modes/vm.js';
import { memoryScenarios } from './frames/memory.js';
import { diagnosticScenarios } from './diagnostics.js';
import { returnScenarios } from './frames/returns.js';
import { boundaryScenarios } from './frames/boundaries.js';
import { aliasScenarios } from './frames/aliases.js';
import { cacheReviewScenarios } from './cache-review.js';

export function softwareEventScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    ...taskScenarios(compiler),
    ...realScenarios(compiler),
    ...protectedScenarios(compiler),
    ...ia32eScenarios(compiler),
    ...vmScenarios(compiler),
    ...memoryScenarios(compiler),
    ...diagnosticScenarios(compiler),
    ...returnScenarios(compiler),
    ...boundaryScenarios(compiler),
    ...aliasScenarios(compiler),
    ...cacheReviewScenarios(compiler),
  ];
}
