import type { Scenario } from './fixture.js';
import { arithmeticScenarios } from './arithmetic.js';
import { conversionScenarios } from './conversions.js';
import { literalScenarios } from './operands/literal-packets.js';
import { reuseScenarios } from './reuse.js';
import { loopBoundaryScenarios } from './loop-boundaries.js';
import { controlScenarios } from './controls.js';
import { relationScenarios } from './operands/relations.js';
import { accessScenarios } from './operands/access.js';
import { decodeScenarios } from './operands/decoding.js';
export function floatingScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    ...arithmeticScenarios(compiler),
    ...conversionScenarios(compiler),
    ...literalScenarios(compiler),
    ...reuseScenarios(compiler),
    ...loopBoundaryScenarios(compiler),
    ...controlScenarios(compiler),
    ...relationScenarios(compiler),
    ...accessScenarios(compiler),
    ...decodeScenarios(compiler),
  ];
}
