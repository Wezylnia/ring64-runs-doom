import { rawActivationScenarios } from './mode-transitions/raw-activation.js';
import { negativeScenarios } from './control-registers/validation.js';
import { systemScenarios } from './system.js';
import { pagingSwitchScenarios } from './mode-transitions/paging.js';
import { boundaryScenarios } from './mode-transitions/boundaries.js';
import { transitionScenarios } from './mode-transitions/guest-sequence.js';
import { controlScenarios } from './control-registers/semantics.js';
import { lifecycleScenarios } from './lifecycle.js';
import { legacyLinuxScenarios } from './mode-transitions/linux.js';
import { lmswScenarios } from './control-registers/lmsw.js';
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
