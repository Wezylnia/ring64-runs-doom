import { matrixScenarios } from './matrix.js';
import { classScenarios } from './classes.js';
import { boundaryScenarios } from './boundaries.js';
import { largePageScenarios } from './large-pages.js';

export function alignmentCheckingScenarios(compiler: WebAssembly.Module) {
  return [
    ...matrixScenarios(compiler),
    ...classScenarios(compiler),
    ...boundaryScenarios(compiler),
    ...largePageScenarios(compiler),
  ];
}
