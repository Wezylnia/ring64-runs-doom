import type { Cpu } from '../cpu.js';
import {
  LOCAL_METHODS,
  type LocalMethod,
  type ParallelRunResult,
  type RunOptions,
} from './protocol.js';

export type ParallelCpu = {
  readonly id: number;
  run(options: RunOptions): Promise<ParallelRunResult>;
  pause(): Promise<void>;
  requestInterrupt(vector: number): Promise<boolean>;
  requestNmi(): Promise<void>;
} & {
  [K in LocalMethod]: (...args: Parameters<Cpu[K]>) => Promise<Awaited<ReturnType<Cpu[K]>>>;
};

export function handle(
  actions: Omit<ParallelCpu, LocalMethod>,
  invoke: (method: LocalMethod, args: unknown[]) => Promise<unknown>,
): ParallelCpu {
  const methods = Object.fromEntries(
    LOCAL_METHODS.map((method) => [method, (...args: unknown[]) => invoke(method, args)]),
  );
  return Object.freeze(Object.assign(actions, methods)) as unknown as ParallelCpu;
}
