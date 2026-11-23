import { Core } from '../../sdk/src/wasm/core.js';

type ArtifactImports = Record<string, WebAssembly.ImportValue>;

export function observeArtifactImports(
  observe: (env: ArtifactImports, observed: boolean) => void,
): () => void {
  const prototype = Core.prototype as unknown as {
    artifactImports(observed: boolean): ArtifactImports;
  };
  const original = prototype.artifactImports;
  prototype.artifactImports = function (observed) {
    const env = original.call(this, observed);
    observe(env, observed);
    return env;
  };
  return () => {
    prototype.artifactImports = original;
  };
}
