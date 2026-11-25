import type { MachineFactory } from './fixture.js';
import { parallelMemoryScenarios } from './memory.js';
import { parallelCoherenceScenarios } from './coherence.js';
import { parallelLifecycleScenarios } from './lifecycle.js';
import { parallelLitmusScenarios } from './litmus.js';
import { parallelPagingMemoryScenarios } from './paging-memory.js';

export async function runParallelScenarios(
  factory: MachineFactory,
  source: BufferSource | WebAssembly.Module,
  selected?: string,
): Promise<{ passed: string[] }> {
  const compiler =
    source instanceof WebAssembly.Module ? source : await WebAssembly.compile(source);
  const scenarios = [
    ...parallelMemoryScenarios(factory, compiler),
    ...parallelPagingMemoryScenarios(factory, compiler),
    ...parallelLitmusScenarios(factory, compiler),
    ...parallelCoherenceScenarios(factory, compiler),
    ...parallelLifecycleScenarios(factory, compiler),
  ].filter(({ name }) => selected === undefined || name === selected);
  if (scenarios.length === 0) throw new Error(`parallel scenario not found: ${selected}`);
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
