import { rawActivationScenarios } from './raw-activation.js';
import { negativeScenarios } from './negative.js';
import { systemScenarios } from './system.js';
import { pagingSwitchScenarios } from './paging-switch.js';
import { boundaryScenarios } from './boundaries.js';
import { transitionScenarios } from './transitions.js';
import { controlScenarios } from './controls.js';
import { lifecycleScenarios } from './lifecycle.js';
import { legacyLinuxScenarios } from './legacy-linux.js';
import { lmswScenarios } from './lmsw.js';
export const resetScenarios = (compiler: WebAssembly.Module) => [
  ...legacyLinuxScenarios(compiler),
  ...lmswScenarios(compiler),
  ...transitionScenarios(compiler),
  ...lifecycleScenarios(compiler),
  ...controlScenarios(compiler),
  ...boundaryScenarios(compiler),
  ...pagingSwitchScenarios(compiler),
  ...systemScenarios(compiler),
  ...negativeScenarios(compiler),
  ...rawActivationScenarios(compiler),
];
