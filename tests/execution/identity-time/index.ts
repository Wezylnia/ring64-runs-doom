import { createCpu } from '../../../sdk/src/index.js';
import type { Scenario } from '../../support/fixture.js';
import { checkIdentityTime, groups } from './oracle.js';
import { identityBoundaryScenarios } from './boundaries.js';

export function identityTimeScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    ...groups.map((group) => ({
      name: `processor identity and virtual time independent ${group} packets`,
      async run() {
        await checkIdentityTime({ createCpu, compiler }, [group]);
      },
    })),
    ...identityBoundaryScenarios(compiler),
  ];
}
