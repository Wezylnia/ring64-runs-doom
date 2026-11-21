import { compoundMatrixScenarios } from './matrix.js';
import { compoundLiteralScenarios } from './literals.js';
import { compoundDecodeScenarios } from './decode.js';
import { compoundAliasScenarios } from './aliases.js';
import { compoundPagingScenarios } from './paging.js';
import { compoundSegmentScenarios } from './segments.js';
import { compoundEventScenarios } from './events.js';
import { compoundLifecycleScenarios } from './lifecycle.js';
import { compoundVmScenarios } from './vm.js';
import { compoundOrderingScenarios } from './ordering.js';
import { compoundModelScenarios } from './models.js';

export function compoundStackScenarios(core: WebAssembly.Module) {
  return [
    ...compoundMatrixScenarios(core),
    ...compoundLiteralScenarios(core),
    ...compoundDecodeScenarios(core),
    ...compoundAliasScenarios(core),
    ...compoundPagingScenarios(core),
    ...compoundSegmentScenarios(core),
    ...compoundEventScenarios(core),
    ...compoundLifecycleScenarios(core),
    ...compoundVmScenarios(core),
    ...compoundOrderingScenarios(core),
    ...compoundModelScenarios(core),
  ];
}
