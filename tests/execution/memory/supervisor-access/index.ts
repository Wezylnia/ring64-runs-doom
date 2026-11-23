import { createCpu } from '../../../../sdk/src/index.js';
import type { Scenario } from '../../../support/fixture.js';
import { checkSupervisorAccess, groups } from './oracle.js';
import { supervisorFlagScenarios } from './permissions/flags.js';
import { supervisorIoScenarios } from './operands/io.js';
import { supervisorNumericScenarios } from './operands/numeric.js';
import { supervisorControlScenarios } from './permissions/controls.js';
import { supervisorProfileScenarios } from './permissions/profiles.js';
import { supervisorContextScenarios } from './permissions/contexts.js';
import { supervisorMetadataScenarios } from './operands/metadata.js';
import { supervisorBoundaryScenarios } from './operands/boundaries.js';
import { supervisorFlowScenarios } from './delivery/flow.js';
import { supervisorPositiveScenarios } from './operands/positive.js';
import { supervisorNoWorkScenarios } from './delivery/no-work.js';
import { supervisorOrderingScenarios } from './delivery/ordering.js';
import { supervisorEventScenarios } from './delivery/events.js';
import { supervisorEntryScenarios } from './delivery/entry.js';

export function supervisorAccessScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    ...groups.map((group) => ({
      name: `supervisor access protection independent ${group} packets`,
      async run() {
        await checkSupervisorAccess({ createCpu, compiler }, [group]);
      },
    })),
    ...supervisorFlagScenarios(compiler),
    ...supervisorIoScenarios(compiler),
    ...supervisorNumericScenarios(compiler),
    ...supervisorControlScenarios(compiler),
    ...supervisorProfileScenarios(compiler),
    ...supervisorContextScenarios(compiler),
    ...supervisorMetadataScenarios(compiler),
    ...supervisorBoundaryScenarios(compiler),
    ...supervisorFlowScenarios(compiler),
    ...supervisorPositiveScenarios(compiler),
    ...supervisorNoWorkScenarios(compiler),
    ...supervisorOrderingScenarios(compiler),
    ...supervisorEventScenarios(compiler),
    ...supervisorEntryScenarios(compiler),
  ];
}
