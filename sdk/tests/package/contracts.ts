import { createCpu, loadCpuCompiler, type Cpu, type Options } from 'ring64';
import {
  createMachine,
  loadParallelCompiler,
  type Machine,
  type MachineOptions,
} from 'ring64/parallel';
// @ts-expect-error Internal modules have no package export.
import type { Core } from 'ring64/wasm/core.js';

const cpuCompiler: Promise<WebAssembly.Module> = loadCpuCompiler();
const parallelCompiler: Promise<WebAssembly.Module> = loadParallelCompiler();
void [cpuCompiler, parallelCompiler];

function checkCpu(options: Options, cpu: Cpu): Promise<Cpu> {
  // @ts-expect-error Budgets select exact integer work in bigint units.
  void cpu.run({ budget: 1 });
  // @ts-expect-error A scalar address uses bigint, not number.
  void cpu.readMemory(0, 1);
  // @ts-expect-error A compiler needs Wasm bytes or a compiled module.
  void createCpu({ ...options, compiler: 'ring64.wasm' });
  return createCpu(options);
}

function checkParallel(options: MachineOptions, machine: Machine): Promise<Machine> {
  // @ts-expect-error A paged vCPU needs its initial paging state.
  void createMachine({ ...options, memoryModel: 'ia32e-paged', cpus: [{ entry: 0n }] });
  // @ts-expect-error A machine owns its readonly list of CPU handles.
  machine.cpus.pop();
  return createMachine(options);
}

export { checkCpu, checkParallel };
export type HiddenCore = Core;
