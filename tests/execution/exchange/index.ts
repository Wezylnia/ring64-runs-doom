import { type Scenario } from './fixture.js';
import { matrixScenarios } from './matrix.js';
import { boundaryScenarios } from './boundaries.js';
import { literalPackets } from './literal-packets.js';
import { taggedHeadScenarios } from './tagged-head.js';
import { wideFaultScenarios } from './wide-faults.js';
import { wideObservationScenarios } from './wide-observation.js';

export function exchangeScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    ...literalPackets(compiler),
    ...taggedHeadScenarios(compiler),
    ...wideFaultScenarios(compiler),
    ...wideObservationScenarios(compiler),
    ...matrixScenarios(compiler),
    ...boundaryScenarios(compiler),
  ];
}
