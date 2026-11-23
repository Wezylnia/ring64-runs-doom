import { type Scenario } from './fixture.js';
import { literalScenarios } from './literals.js';
import { widthScenarios } from './widths.js';
import { branchScenarios, callReturnPriorityScenario } from './branches.js';
import { stackLoopScenarios, stackPointerScenarios } from './stack.js';
import { segmentAccessScenarios, longModeSegmentFaultScenario } from './segments.js';
import { fetchScenarios } from './fetch.js';
export function modeScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    ...literalScenarios(compiler),
    ...widthScenarios(compiler),
    ...branchScenarios(compiler),
    ...stackLoopScenarios(compiler),
    ...segmentAccessScenarios(compiler),
    ...stackPointerScenarios(compiler),
    longModeSegmentFaultScenario(compiler),
    callReturnPriorityScenario(compiler),
    ...fetchScenarios(compiler),
  ];
}
