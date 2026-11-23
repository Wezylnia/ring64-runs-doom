import { type Scenario } from '../../../support/fixture.js';
import { lifecycleScenarios } from './lifecycle.js';
import { boundaryScenarios } from './boundaries.js';
import { cacheScenarios } from './cache.js';
import { siblingEntryScenario } from './sibling-entry.js';

export function fsgsbaseScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    ...lifecycleScenarios(compiler),
    ...boundaryScenarios(compiler),
    ...cacheScenarios(compiler),
    siblingEntryScenario(compiler),
  ];
}
