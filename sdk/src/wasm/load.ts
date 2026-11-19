export async function loadCompiler(url: URL): Promise<WebAssembly.Module> {
  if (url.protocol === 'file:' && typeof process !== 'undefined' && process.versions.node) {
    const { readFile } = process.getBuiltinModule('node:fs/promises');
    return WebAssembly.compile(new Uint8Array(await readFile(url)));
  }
  const response = await fetch(url);
  if (!response.ok) throw new Error(`Could not load Ring64 Wasm (${response.status}): ${url}`);
  return response.headers.get('Content-Type') === 'application/wasm'
    ? WebAssembly.compileStreaming(response)
    : WebAssembly.compile(await response.arrayBuffer());
}
