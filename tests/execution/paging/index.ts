import { pagingControlScenarios } from './controls.js';
import { pagingAccessScenarios } from './access.js';
import { pagingEventScenarios } from './events.js';
import { pagingFetchScenarios } from './fetch.js';
import { pagingFaultScenarios } from './faults.js';
import { pagingReviewFetchScenarios } from './review-fetch.js';
import { pagingReviewAccessScenarios } from './review-access.js';
import { guardedPrefixScenarios } from './guarded-prefixes.js';
import { scalarReplayScenarios } from './scalar-replay.js';
import { writeReplayScenarios } from './write-replay.js';
import { indexedReadLoopScenarios } from './indexed-read-loops.js';
import { byteReadLoopScenarios } from './byte-read-loops.js';
import { translationLifecycleScenarios } from './translation-lifecycle.js';
import { pagingCurrentWalkReviewScenarios } from './review-current-walk.js';
import { pagingMutationOracleScenarios } from './mutation-oracles.js';
import { pagingPermissionReviewScenarios } from './review-permissions.js';
import { invpcidScenarios } from './invpcid.js';
import { pagingRetentionLifecycleScenarios } from './retention-lifecycle.js';
import { pagedRegionScenarios } from './regions.js';
import { pagedRegisterRecurrenceScenarios } from './register-recurrence.js';
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
    ...guardedPrefixScenarios(compiler),
    ...scalarReplayScenarios(compiler),
    ...writeReplayScenarios(compiler),
    ...indexedReadLoopScenarios(compiler),
    ...byteReadLoopScenarios(compiler),
    ...translationLifecycleScenarios(compiler),
    ...pagingScalarF32LoopScenarios(compiler),
    ...pagingCurrentWalkReviewScenarios(compiler),
    ...pagingMutationOracleScenarios(compiler),
    ...pagingPermissionReviewScenarios(compiler),
    ...invpcidScenarios(compiler),
    ...pagingRetentionLifecycleScenarios(compiler),
    ...retainedTranslationScenarios(compiler),
    ...pagedRegionScenarios(compiler),
    ...pagedRegisterRecurrenceScenarios(compiler),
  ];
}
