import { literalScenarios } from './literals.js';
import { imageScenarios } from './images.js';
import { boundaryScenarios } from './boundaries.js';
import { unpackMemoryScenario } from './unpack.js';
import { restoreScenarios } from './restore.js';
import { optimizedScenarios } from './optimized.js';
import type { Scenario } from '../../processor/modes/fixture.js';

export function numericStateScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    ...literalScenarios(compiler),
    ...imageScenarios(compiler),
    ...boundaryScenarios(compiler),
    unpackMemoryScenario(compiler),
    ...restoreScenarios(compiler),
    ...optimizedScenarios(compiler),
  ];
}
