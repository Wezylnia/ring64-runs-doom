import { createCpu, loadCpuCompiler } from '../assets/ring64/index.js';

const codeBase = 0x2000000n;
const stop = 0x8000n;
const state = 0x10000n;
const outputLength = 192 * 108 * 2;
const limits = {
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
  if (!response.ok) throw new Error('Could not load the Wet Ink x86-64 program.');
  return new Uint8Array(await response.arrayBuffer());
}

async function start() {
  const [compiler, program] = await Promise.all([loadCpuCompiler(), loadProgram()]);
  return createCpu({
    compiler,
    limits,
    code: { base: codeBase, bytes: program },
    ram: { base: state, bytes: memory },
  });
}

let cpu;
let tick = 0;
self.onmessage = async ({ data }) => {
  try {
    cpu ??= await start();
    const started = performance.now();
    const registers = Array(16).fill(0n);
    // The flat image starts at step and returns to the sentinel on its SysV stack.
    registers[1] = BigInt(data.y);
    registers[2] = BigInt(data.x);
    registers[4] = state + BigInt(stackOffset);
    registers[6] = BigInt(tick);
    registers[7] = state;
    registers[8] = BigInt(data.previous);
    registers[9] = BigInt(data.active);
    cpu.setState({ registers, flags: 2n, rip: codeBase });
    const result = await cpu.run({ budget: 24_000_000n, stop });
    if (result.reason !== 'stop' || result.state.rip !== stop)
      throw new Error(`Wet Ink stopped at ${result.state.rip.toString(16)} (${result.reason}).`);
    const pigments = cpu.readMemory(state, outputLength);
    self.postMessage({ pigments, tick: tick++, ms: performance.now() - started }, [
      pigments.buffer,
    ]);
  } catch (error) {
    self.postMessage({ error: error instanceof Error ? error.message : String(error) });
  }
};
