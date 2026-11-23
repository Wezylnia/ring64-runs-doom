import { controlScenarios } from './controls.js';
import { imageScenarios } from './images.js';
import { boundaryScenarios } from './boundaries.js';
import type { Scenario } from './fixture.js';

export function xstateScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    ...controlScenarios(compiler),
    ...imageScenarios(compiler),
    ...boundaryScenarios(compiler),
  ];
}
