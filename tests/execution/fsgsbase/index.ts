import { type Scenario } from '../../support/fixture.js';
import { lifecycleScenarios } from './lifecycle.js';
import { boundaryScenarios } from './boundaries.js';

export function fsgsbaseScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [...lifecycleScenarios(compiler), ...boundaryScenarios(compiler)];
}
