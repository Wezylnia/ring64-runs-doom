import { createCpu, loadCpuCompiler, type Limits } from 'ring64';

const entry = 0x1000n;
// mov eax, 40; add eax, 2
const code = Uint8Array.of(0xb8, 0x28, 0x00, 0x00, 0x00, 0x83, 0xc0, 0x02);
const limits = {
  maxInstructions: 128,
  maxSourceBytes: 2048,
  maxArtifactBytes: 262144,
  maxCachedArtifacts: 32,
  maxCachedBytes: 4194304,
  maxTranslationsPerRun: 32,
} satisfies Limits;

const cpu = await createCpu({
  compiler: await loadCpuCompiler(),
  code: { base: entry, bytes: code },
  ram: { base: 0x8000n, bytes: new Uint8Array(32) },
  limits,
});

try {
  const result = await cpu.run({ budget: 10n, stop: entry + BigInt(code.length) });
  if (result.reason !== 'stop') throw new Error(`guest stopped with ${result.reason}`);
  console.log(result.state.registers[0]); // rax = 42n
} finally {
  cpu.dispose();
}
