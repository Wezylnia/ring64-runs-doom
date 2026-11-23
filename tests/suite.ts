import { resetScenarios } from './execution/reset/index.js';
import { ioScenarios } from './execution/io/index.js';
import { mmioScenarios } from './execution/mmio/index.js';
import { numericControlScenarios } from './execution/numeric/controls.js';
import { integerScenarios } from './execution/instructions/integer/index.js';
import { stackScenarios } from './execution/instructions/stack/index.js';
import { addressingScenarios } from './execution/memory/addressing/index.js';
import { memoryScenarios } from './execution/memory/operands.js';
import { mappedMemoryScenarios } from './execution/memory/mappings/index.js';
import { controlScenarios } from './execution/control.js';
import { translationsScenarios } from './execution/translations.js';
import { embeddingScenarios } from '../sdk/tests/api/scenarios.js';
import { modeScenarios } from './execution/modes/index.js';
import { stringScenarios } from './execution/instructions/strings/index.js';
import { nearControlScenarios } from './execution/near-control/index.js';
import { exchangeScenarios } from './execution/instructions/exchange/index.js';
import { simdScenarios } from './execution/numeric/simd/index.js';
import { floatingScenarios } from './execution/numeric/floating/index.js';
import { x87Scenarios } from './execution/numeric/x87/index.js';
import { numericStateScenarios } from './execution/numeric/mmx/index.js';
import { farControlScenarios } from './execution/far-control/index.js';
import { taskSwitchingScenarios } from './execution/task-switching/index.js';
import { callGateScenarios } from './execution/call-gates/index.js';
import { protectionScenarios } from './execution/protection/index.js';
import { streamingScenarios } from './execution/numeric/streaming/index.js';
import { systemFlagScenarios } from './execution/system-flags/index.js';
import { softwareEventScenarios } from './execution/software-events/index.js';
import { exceptionDeliveryScenarios } from './execution/exception-delivery/index.js';
import { externalInterruptScenarios } from './execution/external-interrupts/index.js';

import { accessFaultScenarios } from './execution/memory/access-faults/index.js';

import { hardwareDebugScenarios } from './execution/hardware-debug/index.js';
import { legacyPagingScenarios } from './execution/memory/legacy-paging/index.js';
import { pagingScenarios } from './execution/memory/paging/index.js';
import { fsgsbaseScenarios } from './execution/fsgsbase/index.js';
import { fastSystemScenarios } from './execution/fast-system/index.js';
import { contextReuseScenarios } from './execution/context-reuse/index.js';
import { cr3CodeReuseScenarios } from './execution/cr3-code-reuse/index.js';
import { identityTimeScenarios } from './execution/identity-time/index.js';
import { supervisorAccessScenarios } from './execution/memory/supervisor-access/index.js';
import { alignmentCheckingScenarios } from './execution/memory/alignment-checking/index.js';
import { xstateScenarios } from './execution/numeric/xstate/index.js';
import { protectionKeysScenarios } from './execution/memory/protection-keys/index.js';

export async function runScenarios(
  compiler: Uint8Array | WebAssembly.Module,
): Promise<{ passed: string[] }> {
  const core =
    compiler instanceof WebAssembly.Module
      ? compiler
      : await WebAssembly.compile(new Uint8Array(compiler));
  const scenarios = [
    ...integerScenarios(core),
    ...resetScenarios(core),
    ...stackScenarios(core),
    ...addressingScenarios(core),
    ...memoryScenarios(core),
    ...mappedMemoryScenarios(core),
    ...controlScenarios(core),
    ...translationsScenarios(core),
    ...embeddingScenarios(core),
    ...modeScenarios(core),
    ...stringScenarios(core),
    ...ioScenarios(core),
    ...mmioScenarios(core),
    ...nearControlScenarios(core),
    ...exchangeScenarios(core),
    ...simdScenarios(core),
    ...floatingScenarios(core),
    ...x87Scenarios(core),
    ...numericStateScenarios(core),
    ...streamingScenarios(core),
    ...protectionScenarios(core),
    ...farControlScenarios(core),
    ...callGateScenarios(core),
    ...taskSwitchingScenarios(core),
    ...systemFlagScenarios(core),
    ...softwareEventScenarios(core),
    ...exceptionDeliveryScenarios(core),
    ...externalInterruptScenarios(core),
    ...accessFaultScenarios(core),
    ...hardwareDebugScenarios(core),
    ...pagingScenarios(core),
    ...legacyPagingScenarios(core),
    ...fastSystemScenarios(core),
    ...fsgsbaseScenarios(core),
    ...contextReuseScenarios(core),
    ...cr3CodeReuseScenarios(core),
    ...numericControlScenarios(core),
    ...identityTimeScenarios(core),
    ...supervisorAccessScenarios(core),
    ...alignmentCheckingScenarios(core),
    ...protectionKeysScenarios(core),
    ...xstateScenarios(core),
  ];
  const passed: string[] = [];
  for (const { name, run } of scenarios) {
    try {
      await run();
      passed.push(name);
    } catch (error) {
      throw new Error(`scenario failed: ${name}`, { cause: error });
    }
  }
  return { passed };
}
