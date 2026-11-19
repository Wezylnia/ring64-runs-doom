import { createHash } from 'node:crypto';
import { readFile, readdir } from 'node:fs/promises';
import { relative, resolve, sep } from 'node:path';

export async function emittedModules(directory) {
  return (await readdir(directory, { recursive: true, withFileTypes: true }))
    .filter((entry) => (entry.isFile() || entry.isSymbolicLink()) && entry.name.endsWith('.js'))
    .map((entry) => relative(directory, resolve(entry.parentPath, entry.name)).split(sep).join('/'))
    .sort();
}

export async function hostIdentity(directory) {
  const host = createHash('sha256');
  const productionHost = createHash('sha256');
  const hostFiles = [];
  let hostBytes = 0;
  let productionHostBytes = 0;
  for (const name of await emittedModules(directory)) {
    const bytes = await readFile(resolve(directory, name));
    host.update(name + '\0');
    host.update(bytes);
    hostBytes += bytes.length;
    if (!name.endsWith('.test.js')) {
      productionHost.update(name + '\0');
      productionHost.update(bytes);
      productionHostBytes += bytes.length;
    }
    hostFiles.push({
      name,
      bytes: bytes.length,
      sha256: createHash('sha256').update(bytes).digest('hex'),
    });
  }
  return {
    hostSha256: host.digest('hex'),
    productionHostSha256: productionHost.digest('hex'),
    hostBytes,
    productionHostBytes,
    hostFiles,
  };
}
