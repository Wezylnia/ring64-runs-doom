import { loadCpuCompiler } from 'ring64';

declare global {
  interface Window {
    wetInkCompiler?: Promise<WebAssembly.Module>;
  }
}

const compiler = loadCpuCompiler();
window.wetInkCompiler = compiler;
// The canvas can observe the rejection after the page has loaded.
void compiler.catch(() => {});
