import { createCpu, loadCpuCompiler, type Cpu, type Options, type RunOptions } from 'ring64';
import {
  createMachine,
  loadParallelCompiler,
  type Machine,
  type MachineOptions,
  type RunOptions as ParallelRunOptions,
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
const run: RunOptions = { budget: 1n, stop: 0x1000n };
const parallelRun: ParallelRunOptions = run;
// @ts-expect-error Native dispatch continues inside the SDK.
const dispatch: StandaloneReason = 'dispatch';
// @ts-expect-error Paging restart continues inside the SDK.
const restart: StandaloneReason = 'paging-restart';
// @ts-expect-error Worker slice boundaries continue inside the SDK.
const boundary: ParallelReason = 'parallel-boundary';
// @ts-expect-error The worker's result reason has a closed set of values.
const typo: ParallelReason = 'page-fualt';
void [
  stopped,
  paused,
  publicReason,
  parallelPublicReason,
  parallelRun,
  dispatch,
  restart,
  boundary,
  typo,
];

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

export function checkFaultDiagnostics(result: import('ring64').RunResult): void {
  switch (result.reason) {
    case 'fetch-fault':
    case 'target-fault':
    case 'data-fault':
      void result.fault.address;
      break;
    case 'segment-fault':
      void result.segmentFault.offset;
      break;
    case 'alignment-fault':
      void result.alignmentFault.alignment;
      break;
    case 'alignment-check':
      void result.alignmentCheck.vector;
      break;
    case 'simd-exception':
      void result.simdException.instructionPc;
      break;
    case 'general-protection':
      void result.generalProtectionFault.instructionPc;
      break;
    case 'x87-exception':
      void result.x87Exception.instructionPc;
      break;
    case 'protection-fault':
      void result.protectionFault.instructionPc;
      break;
    case 'system-memory-fault':
      void result.systemMemoryFault.address;
      break;
    case 'debug-exception':
      void result.debugException.resumePc;
      break;
    case 'page-fault':
      void result.pageFault.address;
      break;
    case 'physical-memory-fault':
      void result.physicalMemoryFault.physicalAddress;
      break;
    case 'invalid-encoding':
      // @ts-expect-error An ordinary invalid encoding has no numeric-control diagnostic.
      void result.numericControlFault.vector;
      break;
    case 'pending-exception':
      // @ts-expect-error A pending exception has no guaranteed page-fault diagnostic.
      void result.pageFault.address;
      break;
  }
}

export function checkParallelPageFault(result: import('ring64/parallel').ParallelRunResult): void {
  if (result.reason === 'page-fault') void result.pageFault.address;
}
