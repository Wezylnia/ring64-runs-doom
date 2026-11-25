import { createCpu } from '../../sdk/src/index.js';
import type { loadParallelCompiler } from '../../sdk/src/machine.js';
import { supportsParallelMemory64 } from '../../sdk/src/parallel/memory/ram.js';
import { runParallelScenarios } from '../execution/parallel/index.js';
import {
  ENTRY,
  equal,
  isParallelMemory64,
  linearMachine,
  type MachineFactory,
} from '../execution/parallel/fixture.js';
import { checkParallelMemory64 } from '../execution/parallel/memory64.js';

const page = window as Window & {
  ring64Parallel?: Promise<{ passed: string[]; memory64?: true }>;
};
page.ring64Parallel = (async () => {
  const entry = new URL('../../sdk/src/machine.js', import.meta.url);
  const api: { createMachine: MachineFactory; loadParallelCompiler: typeof loadParallelCompiler } =
    await import(entry.href);
  const response = await fetch('/build/ring64-parallel.wasm');
  if (!response.ok) throw new Error('missing parallel Core Wasm');
  const bytes = await response.arrayBuffer();
  if (location.pathname === '/unisolated') {
    equal(crossOriginIsolated, false, 'nonisolated control');
    const accepted = await api.createMachine(linearMachine(new Uint8Array(bytes), '90')).then(
      async (machine) => {
        await machine.dispose();
        return 'accepted';
      },
      (error: { code?: string }) => error.code,
    );
    equal(accepted, 'unsupported', 'typed parallel capability rejection');
    const solo = await fetch('/build/ring64.wasm');
    const options = linearMachine(new Uint8Array(await solo.arrayBuffer()), '90');
    const cpu = await createCpu({
      compiler: options.compiler,
      memory: options.memory,
      memoryLimits: options.memoryLimits,
      limits: options.limits,
      entry: ENTRY,
    });
    try {
      const done = await cpu.run({ budget: 2n, stop: ENTRY + 1n });
      equal(done.reason, 'stop', 'private CPU remains usable without shared runtime');
      equal(done.retired, 1n, 'one private NOP');
    } finally {
      cpu.dispose();
    }
    return { passed: ['parallel rejects missing isolation while the private CPU still executes'] };
  }
  if (!crossOriginIsolated) throw new Error('worker conformance requires cross-origin isolation');
  const baseline = await runParallelScenarios(api.createMachine, bytes);
  const supported = supportsParallelMemory64();
  const compiler = await api.loadParallelCompiler();
  equal(isParallelMemory64(compiler), supported, 'default parallel profile');
  if (!supported) return baseline;
  const wide = await runParallelScenarios(api.createMachine, compiler);
  await checkParallelMemory64(api.createMachine, compiler);
  return {
    passed: [
      ...baseline.passed,
      ...wide.passed.map((name) => `memory64: ${name}`),
      'memory64: parallel workers share bytes beyond 4 GiB',
    ],
    memory64: true as const,
  };
})();
