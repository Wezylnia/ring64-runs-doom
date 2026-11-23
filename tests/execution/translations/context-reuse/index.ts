import { createCpu } from '../../../../sdk/src/index.js';
import { variantScenarios } from './variants/selection.js';
import { warmScenarios } from './variants/warmup.js';
import { writerScenarios } from './variants/context-writers.js';
import { sourceScenarios } from './invalidation/sources.js';
import { fenceScenarios } from './invalidation/fences.js';
import { capacityScenarios } from './variants/capacity.js';
import { lifecycleScenarios } from './lifecycle.js';
import { boundaryScenarios } from './boundaries.js';
import { reviewBoundaryScenarios } from './event-frontiers.js';
import { reviewPagingScenarios } from './invalidation/root-replacement.js';
import type { Factory, Scenario } from './fixture.js';

export function contextReuseScenarios(
  compiler: WebAssembly.Module,
  make: Factory = createCpu,
): Scenario[] {
  return [
    ...variantScenarios(compiler, make),
    ...warmScenarios(compiler, make),
    ...writerScenarios(compiler, make),
    ...sourceScenarios(compiler, make),
    ...fenceScenarios(compiler, make),
    ...capacityScenarios(compiler, make),
    ...lifecycleScenarios(compiler, make),
    ...boundaryScenarios(compiler, make),
    ...reviewBoundaryScenarios(compiler, make),
    ...reviewPagingScenarios(compiler, make),
  ];
}
