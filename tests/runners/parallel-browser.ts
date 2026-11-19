import { createCpu } from '../../sdk/src/index.js';
import { runParallelScenarios } from '../execution/parallel/index.js';
import { ENTRY, equal, linearMachine, type MachineFactory } from '../execution/parallel/fixture.js';

const page = window as Window & {
  ring64Parallel?: Promise<{ passed: string[] }>;
};
page.ring64Parallel = (async () => {
  const entry = new URL('../../sdk/src/machine.js', import.meta.url);
  const { createMachine }: { createMachine: MachineFactory } = await import(entry.href);
  const response = await fetch('/build/ring64-parallel.wasm');
  if (!response.ok) throw new Error('missing parallel Core Wasm');
  const bytes = await response.arrayBuffer();
  if (location.pathname === '/unisolated') {
    equal(crossOriginIsolated, false, 'nonisolated control');
    const accepted = await createMachine(linearMachine(new Uint8Array(bytes), '90')).then(
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
  return runParallelScenarios(createMachine, bytes);
})();
