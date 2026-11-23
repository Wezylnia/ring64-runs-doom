import { freePascalSinCosScenarios } from './transcendentals/fsincos-fpc.js';
import { fsincosScenarios } from './transcendentals/fsincos.js';
import { muslLogExpScenario } from './transcendentals/log-exp-musl.js';
import { logExpScenarios } from './transcendentals/log-exp.js';
import { muslFpatanScenarios } from './transcendentals/fpatan-musl.js';
import { fpatanScenarios } from './transcendentals/fpatan.js';
import { fsetpmScenarios } from './fsetpm.js';
import { fisttpScenarios } from './fisttp.js';
import type { Scenario } from './fixture.js';
import { literalScenarios } from './formats/literals.js';
import { modeScenarios } from './modes.js';
import { memoryScenarios } from './formats/memory.js';
import { boundaryScenarios } from './boundaries.js';
import { specialScenarios } from './formats/specials.js';
import { specialMemoryScenarios } from './formats/special-memory.js';
import { specialImageScenarios } from './formats/special-images.js';

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
