import { basicGateScenarios } from './transfers/basic.js';
import { widthScenarios } from './transfers/widths.js';
import { privilegeScenarios } from './transfers/privilege.js';
import { longGateScenarios } from './transfers/long64.js';
import { gateDebugScenarios } from './debug.js';
import { gateCanonicalScenarios } from './transfers/canonical.js';
import { gatePagingScenarios } from './publication/paging.js';
import { gateLifecycleScenarios } from './publication/lifecycle.js';
import { gateOperandScenarios } from './transfers/operands.js';
import { gatePublicationScenarios } from './publication/dirty-bits.js';
import { gateOwnershipScenarios } from './publication/ownership.js';
import { gateFrontierScenarios } from './publication/frontiers.js';
import { gatePagingBoundaryScenarios } from './publication/paging-boundaries.js';
import type { Scenario } from './fixture.js';

export function callGateScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    ...basicGateScenarios(compiler),
    ...widthScenarios(compiler),
    ...privilegeScenarios(compiler),
    ...longGateScenarios(compiler),
    ...gateDebugScenarios(compiler),
    ...gateCanonicalScenarios(compiler),
    ...gatePagingScenarios(compiler),
    ...gateLifecycleScenarios(compiler),
    ...gateOperandScenarios(compiler),
    ...gatePublicationScenarios(compiler),
    ...gateOwnershipScenarios(compiler),
    ...gateFrontierScenarios(compiler),
    ...gatePagingBoundaryScenarios(compiler),
  ];
}
