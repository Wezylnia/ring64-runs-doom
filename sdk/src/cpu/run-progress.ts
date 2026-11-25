import type { Core } from '../wasm/core.js';

export function validateNativeProgress(
  reason: ReturnType<Core['execute']>,
  budget: bigint,
  repeatBudget: bigint,
  { remaining, repeatRemaining }: ReturnType<Core['current']>,
): void {
  if (
    remaining > budget ||
    repeatRemaining > repeatBudget ||
    (reason === 'dispatch' && remaining === budget && repeatRemaining === repeatBudget) ||
    (reason === 'budget' && (remaining !== 0n || budget === 0n)) ||
    (reason === 'repeat-budget' && (repeatRemaining !== 0n || remaining === 0n))
  ) {
    throw new Error('generated execution violated its progress contract');
  }
}
