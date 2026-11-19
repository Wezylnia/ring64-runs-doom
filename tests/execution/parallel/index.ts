import type { MachineFactory } from './fixture.js';
import { parallelMemoryScenarios } from './memory.js';
import { parallelCoherenceScenarios } from './coherence.js';
import { parallelLifecycleScenarios } from './lifecycle.js';

export async function runParallelScenarios(
  factory: MachineFactory,
  bytes: BufferSource,
): Promise<{ passed: string[] }> {
  const compiler = await WebAssembly.compile(bytes);
  const scenarios = [
    ...parallelMemoryScenarios(factory, compiler),
    ...parallelCoherenceScenarios(factory, compiler),
    ...parallelLifecycleScenarios(factory, compiler),
  ];
  const passed: string[] = [];
  for (const { name, run } of scenarios) {
    try {
      await run();
      passed.push(name);
    } catch (cause) {
      throw new Error(`parallel scenario failed: ${name}`, { cause });
    }
  }
  return { passed };
}
