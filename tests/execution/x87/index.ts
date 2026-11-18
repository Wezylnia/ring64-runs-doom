import { freePascalSinCosScenarios } from './fsincos-fpc.js';
import { fsincosScenarios } from './fsincos.js';
import { muslLogExpScenario } from './log-exp-musl.js';
import { logExpScenarios } from './log-exp.js';
import { muslFpatanScenarios } from './fpatan-musl.js';
import { fpatanScenarios } from './fpatan.js';
import { fsetpmScenarios } from './fsetpm.js';
import { fisttpScenarios } from './fisttp.js';
import type { Scenario } from './fixture.js';
import { literalScenarios } from './literals.js';
import { modeScenarios } from './modes.js';
import { memoryScenarios } from './memory.js';
import { boundaryScenarios } from './boundaries.js';
import { specialScenarios } from './specials.js';
import { specialMemoryScenarios } from './special-memory.js';
import { specialImageScenarios } from './special-images.js';

export function x87Scenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    ...freePascalSinCosScenarios(compiler),
    ...fsincosScenarios(compiler),
    muslLogExpScenario(compiler),
    ...logExpScenarios(compiler),
    ...muslFpatanScenarios(compiler),
    ...fpatanScenarios(compiler),
    ...fsetpmScenarios(compiler),
    ...fisttpScenarios(compiler),
    ...literalScenarios(compiler),
    ...specialScenarios(compiler),
    ...specialMemoryScenarios(compiler),
    ...specialImageScenarios(compiler),
    ...modeScenarios(compiler),
    ...memoryScenarios(compiler),
    ...boundaryScenarios(compiler),
  ];
}
