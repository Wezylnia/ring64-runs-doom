import { createCpu, loadCpuCompiler, type Cpu, type Limits, type Registers } from 'ring64';
import { pigmentHeight, pigmentWidth, type StepReply, type StepRequest } from './protocol.js';

declare const self: DedicatedWorkerGlobalScope;

const codeBase = 0x2000000n;
const stop = 0x8000n;
const state = 0x10000n;
const outputLength = pigmentWidth * pigmentHeight * 2;
const origin = {
  x: Math.floor(Math.random() * 65536),
  y: Math.floor(Math.random() * 65536),
};
const limits: Limits = {
  maxInstructions: 512,
  maxSourceBytes: 8192,
  maxArtifactBytes: 1048576,
  maxCachedArtifacts: 256,
  maxCachedBytes: 16777216,
  maxTranslationsPerRun: 256,
};
const memory = new Uint8Array(0x90000);
const stackOffset = memory.length - 8;
new DataView(memory.buffer).setBigUint64(stackOffset, stop, true);

async function loadProgram() {
  const response = await fetch(new URL('../assets/wet-ink/wet-ink.bin', import.meta.url));
  if (!response.ok) throw new Error('could not load the wet ink x86-64 program.');
  return new Uint8Array(await response.arrayBuffer());
}

async function start(): Promise<Cpu> {
  const [compiler, program] = await Promise.all([loadCpuCompiler(), loadProgram()]);
  return createCpu({
    compiler,
    limits,
    code: { base: codeBase, bytes: program },
    ram: { base: state, bytes: memory },
  });
}

let ready: Promise<Cpu> | undefined;
let tick = 0;
self.onmessage = async ({ data }: MessageEvent<StepRequest>) => {
  try {
    const cpu = await (ready ??= start());
    const started = performance.now();
    // The flat image starts at step and returns to the sentinel on its SysV stack.
    const registers: Registers = [0n, 0n, 0n, 0n, 0n, 0n, 0n, 0n, 0n, 0n, 0n, 0n, 0n, 0n, 0n, 0n];
    const position = tick === 0 ? origin : data;
    registers[1] = BigInt(position.y); // RCX
    registers[2] = BigInt(position.x); // RDX
    registers[4] = state + BigInt(stackOffset); // RSP
    registers[6] = BigInt(tick); // RSI
    registers[7] = state; // RDI
    registers[8] = BigInt(data.previous); // R8
    registers[9] = BigInt(data.active); // R9
    cpu.setState({ registers, flags: 2n, rip: codeBase });
    const result = await cpu.run({ budget: 24_000_000n, stop });
    if (result.reason !== 'stop' || result.state.rip !== stop)
      throw new Error(`wet ink stopped at ${result.state.rip.toString(16)} (${result.reason}).`);
    const pigments = cpu.readMemory(state, outputLength);
    const frame: StepReply = { pigments, tick: tick++, ms: performance.now() - started };
    self.postMessage(frame, [pigments.buffer]);
  } catch (error) {
    const failure: StepReply = { error: error instanceof Error ? error.message : String(error) };
    self.postMessage(failure);
  }
};
