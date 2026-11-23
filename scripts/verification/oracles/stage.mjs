import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { hash, root } from '../artifacts.mjs';

// Keep the authored oracle's relative imports intact beside the SDK being measured.
export async function stageOracle(snapshot, sourceHashes) {
  const modules = Object.entries(sourceHashes).filter(([name]) => name.startsWith('build/tests/'));
  for (const [name, expected] of modules) {
    const source = resolve(root, name);
    const destination = resolve(snapshot, name);
    const bytes = await readFile(source);
    if (hash(bytes) !== expected) throw Error(`oracle changed before staging: ${name}`);
    if (destination !== source) {
      await mkdir(dirname(destination), { recursive: true });
      await writeFile(destination, bytes);
    }
  }
  return async function verifyStagedOracle() {
    for (const [name, expected] of modules) {
      if (hash(await readFile(resolve(snapshot, name))) !== expected)
        throw Error(`frozen oracle changed during audit: ${name}`);
    }
  };
}
