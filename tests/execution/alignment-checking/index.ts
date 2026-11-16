import { matrixScenarios } from './matrix.js';
import { classScenarios } from './classes.js';

export function alignmentCheckingScenarios(compiler: WebAssembly.Module) {
  return [...matrixScenarios(compiler), ...classScenarios(compiler)];
}
