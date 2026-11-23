import type { MemoryModel } from '../embedding/memory.js';
import { captureContext, type ExecutionContext } from '../state/system/context.js';
import type { CompileLimits } from '../wasm/core.js';

export interface Limits extends CompileLimits {
  maxCachedArtifacts: number;
  maxCachedBytes: number;
  maxTranslationsPerRun: number;
}

export function captureLimits(input: Limits): Limits {
  function read(name: keyof Limits) {
    const value = input[name];
    if (!Number.isSafeInteger(value) || value <= 0 || value > 0xffffffff)
      throw new TypeError(`${name} must be an explicit positive wasm32 integer`);
    return value;
  }
  return {
    maxInstructions: read('maxInstructions'),
    maxSourceBytes: read('maxSourceBytes'),
    maxArtifactBytes: read('maxArtifactBytes'),
    maxCachedArtifacts: read('maxCachedArtifacts'),
    maxCachedBytes: read('maxCachedBytes'),
    maxTranslationsPerRun: read('maxTranslationsPerRun'),
  };
}

export function captureInitialContext(
  contextInput: ExecutionContext | undefined,
  memoryModel: MemoryModel | undefined,
): ExecutionContext | undefined {
  const context =
    contextInput === undefined
      ? undefined
      : captureContext(contextInput, memoryModel === 'physical');
  if (memoryModel === 'physical' && context === undefined)
    throw new TypeError('physical memory requires a non-undefined context');
  if (
    memoryModel === 'ia32e-paged' &&
    context &&
    context.mode !== 'long64' &&
    context.mode !== 'compatibility'
  )
    throw new TypeError('paged memory requires a Long64 or compatibility context');
  return context;
}
