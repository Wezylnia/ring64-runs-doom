import { pagingControlScenarios } from './controls.js';
import { pagingAccessScenarios } from './access.js';
import { pagingEventScenarios } from './events.js';
import { pagingFetchScenarios } from './fetch.js';
import { pagingFaultScenarios } from './faults.js';
import { pagingReviewFetchScenarios } from './review-fetch.js';
import { pagingReviewAccessScenarios } from './review-access.js';
import { pagingBoundaryReviewScenarios } from './review-boundaries.js';
import { pagingCurrentWalkReviewScenarios } from './review-current-walk.js';
import { pagingMutationOracleScenarios } from './mutation-oracles.js';
import { pagingPermissionReviewScenarios } from './review-permissions.js';
import { invpcidScenarios } from './invpcid.js';
import { pagingRetentionLifecycleScenarios } from './retention-lifecycle.js';
import { retainedTranslationScenarios } from './retained-translations.js';
import { pagingScalarF32LoopScenarios } from './scalar-f32-loop.js';
import type { Scenario } from '../../support/fixture.js';

export function pagingScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    ...pagingControlScenarios(compiler),
    ...pagingAccessScenarios(compiler),
    ...pagingEventScenarios(compiler),
    ...pagingFetchScenarios(compiler),
    ...pagingFaultScenarios(compiler),
    ...pagingReviewFetchScenarios(compiler),
    ...pagingReviewAccessScenarios(compiler),
    ...pagingBoundaryReviewScenarios(compiler),
    ...pagingScalarF32LoopScenarios(compiler),
    ...pagingCurrentWalkReviewScenarios(compiler),
    ...pagingMutationOracleScenarios(compiler),
    ...pagingPermissionReviewScenarios(compiler),
    ...invpcidScenarios(compiler),
    ...pagingRetentionLifecycleScenarios(compiler),
    ...retainedTranslationScenarios(compiler),
  ];
}
