import { createCpu, loadCpuCompiler, type Cpu, type Options } from 'ring64';
import {
  createMachine,
  loadParallelCompiler,
  type Machine,
  type MachineOptions,
} from 'ring64/parallel';
// @ts-expect-error Internal modules have no package export.
import type { Core } from 'ring64/wasm/core.js';

type StandaloneReason = Awaited<ReturnType<Cpu['run']>>['reason'];
type ParallelReason = Awaited<ReturnType<Machine['cpus'][number]['run']>>['reason'];
type RunReason = import('ring64').RunReason;
type ParallelRunReason = import('ring64/parallel').RunReason;

const stopped: StandaloneReason = 'stop';
const paused: ParallelReason = 'paused';
const publicReason: RunReason = stopped;
const parallelPublicReason: ParallelRunReason = paused;
// @ts-expect-error Native dispatch continues inside the SDK.
const dispatch: StandaloneReason = 'dispatch';
// @ts-expect-error Paging restart continues inside the SDK.
const restart: StandaloneReason = 'paging-restart';
// @ts-expect-error Worker slice boundaries continue inside the SDK.
const boundary: ParallelReason = 'parallel-boundary';
// @ts-expect-error The worker's result reason has a closed set of values.
const typo: ParallelReason = 'page-fualt';
void [stopped, paused, publicReason, parallelPublicReason, dispatch, restart, boundary, typo];

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

async function checkRegisterCount(cpu: Cpu | Machine['cpus'][number]): Promise<16> {
  const state = await cpu.readState();
  // @ts-expect-error A scalar state must supply all sixteen registers.
  await cpu.setState({ ...state, registers: [0n] });
  // @ts-expect-error The scalar register bank ends at R15.
  void state.registers[16];
  return state.registers.length;
}

export { checkRegisterCount };
