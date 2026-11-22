import type { GeneratedRun } from '../wasm/abi.js';
import type { CompiledUnitMetadata, Core, UnitMetadata } from '../wasm/core.js';

export interface Artifact {
  run: GeneratedRun;
  metadata: CompiledUnitMetadata;
}

export interface InstalledEntry {
  readonly artifact: Artifact;
  readonly index: number;
}

interface Recency {
  readonly artifact: Artifact;
  older: Recency | undefined;
  newer: Recency | undefined;
}

const EMPTY: readonly InstalledEntry[] = Object.freeze([]);

export class Translations {
  readonly #entries = new Map<bigint, readonly InstalledEntry[]>();
  readonly #observedEntries = new Map<bigint, readonly InstalledEntry[]>();
  readonly #artifacts = new Map<Artifact, Recency>();
  readonly #preferred = new WeakMap<Artifact, ReadonlySet<bigint>>();
  #oldest: Recency | undefined;
  #newest: Recency | undefined;
  #bytes = 0;

  constructor(
    readonly maxArtifacts: number,
    readonly maxBytes: number,
    readonly release: (artifact: Artifact) => void = () => {},
  ) {}

  candidates(pc: bigint, boundaryObserved = false): readonly InstalledEntry[] {
    const entries = boundaryObserved ? this.#observedEntries : this.#entries;
    return entries.get(pc) ?? EMPTY;
  }

  touch(artifact: Artifact): void {
    const entry = this.#artifacts.get(artifact);
    if (!entry || entry === this.#newest) return;
    this.#detach(entry);
    this.#append(entry);
  }

  #append(entry: Recency): void {
    entry.older = this.#newest;
    entry.newer = undefined;
    if (this.#newest) this.#newest.newer = entry;
    else this.#oldest = entry;
    this.#newest = entry;
  }

  #detach(entry: Recency): void {
    const { older, newer } = entry;
    if (older) older.newer = newer;
    else this.#oldest = newer;
    if (newer) newer.older = older;
    else this.#newest = older;
  }

  *#ordered(): IterableIterator<Artifact> {
    let entry = this.#oldest;
    while (entry) {
      const { newer, artifact } = entry;
      yield artifact;
      entry = newer;
    }
  }

  canFit(bytes: number): boolean {
    return bytes <= this.maxBytes;
  }

  installCompiled(
    core: Core,
    module: WebAssembly.Module,
    metadata: CompiledUnitMetadata,
    timings: { instantiationMs: number },
    preferred?: ReadonlySet<bigint>,
  ): Artifact | 'resource-limit' | undefined {
    if (
      core.contextVersion() !== metadata.contextVersion ||
      core.boundaryObserved() !== (metadata.boundaryObserved ?? false) ||
      !core.valid(metadata.owner)
    )
      return undefined;

    const began = performance.now();
    const artifact: Artifact = {
      run: core.instantiate(module, metadata.boundaryObserved),
      metadata,
    };
    timings.instantiationMs += performance.now() - began;
    if (!core.publish(metadata.owner)) return undefined;
    return this.install(artifact, preferred) ? artifact : 'resource-limit';
  }

  install(artifact: Artifact, preferred?: ReadonlySet<bigint>): boolean {
    if (!this.canFit(artifact.metadata.byteLength)) return false;
    if (this.#artifacts.has(artifact)) return true;
    while (
      this.#artifacts.size >= this.maxArtifacts ||
      this.#bytes + artifact.metadata.byteLength > this.maxBytes
    ) {
      const oldest = this.#oldest?.artifact;
      if (!oldest) throw new Error('cache accounting failed');
      this.remove(oldest);
    }
    const recency: Recency = { artifact, older: undefined, newer: undefined };
    this.#artifacts.set(artifact, recency);
    this.#append(recency);
    this.#bytes += artifact.metadata.byteLength;
    if (preferred) this.#preferred.set(artifact, preferred);
    const entries = artifact.metadata.boundaryObserved ? this.#observedEntries : this.#entries;
    artifact.metadata.entries.forEach((pc, index) => {
      const installed = Object.freeze({ artifact, index });
      const previous = entries.get(pc) ?? EMPTY;
      if (preferred?.has(pc)) {
        entries.set(pc, [installed, ...previous]);
      } else if (preferred) {
        entries.set(pc, [...previous, installed]);
      } else {
        const preferredCount = previous.findIndex(
          (entry) => !this.#preferred.get(entry.artifact)?.has(pc),
        );
        const split = preferredCount === -1 ? previous.length : preferredCount;
        entries.set(pc, [...previous.slice(0, split), installed, ...previous.slice(split)]);
      }
    });
    return true;
  }

  resources(): { cachedArtifacts: number; cachedBytes: number } {
    return { cachedArtifacts: this.#artifacts.size, cachedBytes: this.#bytes };
  }

  inspect(): UnitMetadata[] {
    return Array.from(this.#ordered(), (artifact) => {
      const metadata = {
        ...artifact.metadata,
        entries: [...artifact.metadata.entries],
        dependencies: artifact.metadata.dependencies.map((range) => ({ ...range })),
      };
      delete metadata.hasIndirectCall;
      return metadata;
    });
  }

  remove(artifact: Artifact): void {
    const entry = this.#artifacts.get(artifact);
    if (!entry) return;
    this.#artifacts.delete(artifact);
    this.#detach(entry);
    this.#bytes -= artifact.metadata.byteLength;
    const entries = artifact.metadata.boundaryObserved ? this.#observedEntries : this.#entries;
    for (const pc of artifact.metadata.entries) {
      const remaining = entries.get(pc)?.filter((entry) => entry.artifact !== artifact);
      if (remaining?.length) entries.set(pc, remaining);
      else entries.delete(pc);
    }
    this.release(artifact);
  }

  prune(valid: (artifact: Artifact) => boolean): void {
    for (const artifact of this.#ordered()) {
      if (!valid(artifact)) this.remove(artifact);
    }
  }

  clear(): void {
    while (this.#oldest) this.remove(this.#oldest.artifact);
  }
}
