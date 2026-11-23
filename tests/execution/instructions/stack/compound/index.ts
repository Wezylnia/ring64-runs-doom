import { compoundMatrixScenarios } from './operands/matrix.js';
import { compoundLiteralScenarios } from './operands/literals.js';
import { compoundDecodeScenarios } from './operands/decode.js';
import { compoundAliasScenarios } from './access/aliases.js';
import { compoundPagingScenarios } from './access/paging.js';
import { compoundSegmentScenarios } from './access/segments.js';
import { compoundEventScenarios } from './events.js';
import { compoundLifecycleScenarios } from './lifecycle.js';
import { compoundVmScenarios } from './vm.js';
import { compoundOrderingScenarios } from './access/ordering.js';
import { compoundModelScenarios } from './operands/models.js';

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
