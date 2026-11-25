import { supportsGuestMemory64 } from './wasm/guest-memory64.js';

/** Prefer memory64 on supported hosts. Reuse the compiled Core across CPUs. */
export async function loadCpuCompiler(
  guestMemory: 'memory32' | 'memory64' = supportsGuestMemory64() ? 'memory64' : 'memory32',
): Promise<WebAssembly.Module> {
  if (guestMemory !== 'memory32' && guestMemory !== 'memory64')
    throw new TypeError('guestMemory must be memory32 or memory64');
  const { loadCompiler } = await import('./wasm/load.js');
  return loadCompiler(
    guestMemory === 'memory64'
      ? new URL('./ring64-memory64.wasm', import.meta.url)
      : new URL('./ring64.wasm', import.meta.url),
  );
}
