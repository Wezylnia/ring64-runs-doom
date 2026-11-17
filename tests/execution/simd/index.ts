import { type Scenario } from './fixture.js';
import { sse3Scenarios } from './sse3.js';
import { ssse3Scenarios } from './ssse3.js';
import { ssse3BoundaryScenarios } from './ssse3-boundaries.js';
import { registerScenarios } from './registers.js';
import { transferScenarios } from './transfers.js';
import { memoryScenarios } from './memory.js';
import { reuseScenarios } from './reuse.js';
import { boundaryScenarios } from './boundaries.js';
import { literalScenarios } from './literal-packets.js';
import { decodingScenarios } from './decoding.js';
export function simdScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    ...registerScenarios(compiler),
    ...sse3Scenarios(compiler),
    ...ssse3Scenarios(compiler),
    ...ssse3BoundaryScenarios(compiler),
    ...transferScenarios(compiler),
    ...memoryScenarios(compiler),
    ...reuseScenarios(compiler),
    ...boundaryScenarios(compiler),
    ...literalScenarios(compiler),
    ...decodingScenarios(compiler),
  ];
}
