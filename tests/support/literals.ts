import type { Limits, Registers } from '../../sdk/src/index.js';

export interface Scenario {
  name: string;
  run(): Promise<void>;
}

export const LIMITS: Limits = {
  maxInstructions: 128,
  maxSourceBytes: 2048,
  maxArtifactBytes: 262144,
  maxCachedArtifacts: 32,
  maxCachedBytes: 4 * 1024 * 1024,
  maxTranslationsPerRun: 32,
};

export function check(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

export function hex(source: string): Uint8Array {
  check(source.length % 2 === 0 && /^[0-9a-f]*$/.test(source), 'malformed literal bytes');
  return Uint8Array.from(source.match(/../g) ?? [], (byte) => Number.parseInt(byte, 16));
}

export function registerBank(value: bigint | ((index: number) => bigint) = 0n): Registers {
  return (
    typeof value === 'function'
      ? Array.from({ length: 16 }, (_, index) => value(index))
      : Array<bigint>(16).fill(value)
  ) as Registers;
}
