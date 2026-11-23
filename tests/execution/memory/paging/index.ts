import { pagingControlScenarios } from './controls.js';
import { pagingAccessScenarios } from './access/operands.js';
import { pagingEventScenarios } from './access/events.js';
import { pagingFetchScenarios } from './walk/fetch.js';
import { pagingFaultScenarios } from './access/faults.js';
import { pagingReviewFetchScenarios } from './walk/fetch-restarts.js';
import { pagingReviewAccessScenarios } from './access/operand-faults.js';
import { guardedPrefixScenarios } from './replay/guarded-prefixes.js';
import { scalarReplayScenarios } from './replay/scalar.js';
import { writeReplayScenarios } from './replay/writes.js';
import { indexedReadLoopScenarios } from './loops/indexed-reads.js';
import { byteReadLoopScenarios } from './loops/byte-reads.js';
import { translationLifecycleScenarios } from './translations/source-invalidation.js';
import { pagingCurrentWalkReviewScenarios } from './walk/current-hierarchy.js';
import { pagingMutationOracleScenarios } from './access/frame-aliases.js';
import { pagingPermissionReviewScenarios } from './walk/permissions.js';
import { invpcidScenarios } from './translations/invpcid.js';
import { pagingRetentionLifecycleScenarios } from './translations/invalidation.js';
import { pagedRegionScenarios } from './translations/regions.js';
import { pagedRegisterRecurrenceScenarios } from './loops/register-recurrence.js';
import { retainedTranslationScenarios } from './translations/retention.js';
import { pagingScalarF32LoopScenarios } from './loops/scalar-f32.js';
import type { Scenario } from '../../../support/fixture.js';

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
