import type { CompileLimits, UnitMetadata } from '../wasm/core.js';
import type { Artifact, InstalledEntry } from './translations.js';

interface Edge {
  child: UnitMetadata;
  visits: number;
}

export class RegionTrace {
  readonly #targets = new Map<bigint, Edge>();
  readonly #entries: ReadonlySet<bigint>;
  readonly #resumes = new Map<bigint, number>();
  #outgoing: bigint | undefined;
  #samples = 0;
  #returns = 0;

  constructor(
    readonly head: bigint,
    readonly root: UnitMetadata,
  ) {
    this.#entries = new Set(root.entries);
  }

  get boundaryObserved(): boolean | undefined {
    return this.root.boundaryObserved;
  }

  sample(from: bigint, to: bigint, entry: InstalledEntry): boolean {
    const child = entry.artifact.metadata;
    const previous = this.#outgoing;
    this.#outgoing = undefined;
    this.#samples++;
    if (child.owner === this.root.owner) {
      if (to >> 12n === this.head >> 12n && !this.#entries.has(to)) this.#outgoing = to;
    } else if (
      previous === from &&
      child.boundaryObserved === this.root.boundaryObserved &&
      this.#entries.has(to)
    ) {
      let edge = this.#targets.get(from);
      if (!edge) {
        if (this.#targets.size === 16) return this.#samples === 512;
        edge = { child, visits: 0 };
        this.#targets.set(from, edge);
      }
      if (edge.child.owner === child.owner) {
        edge.visits++;
        this.#returns++;
        this.#resumes.set(to, (this.#resumes.get(to) ?? 0) + 1);
      }
    }
    return this.#samples === 512;
  }

  choose(
    limits: CompileLimits,
    valid: (owner: bigint) => boolean,
  ): { entry: bigint; seeds: bigint[] } | undefined {
    if (this.#returns < 128 || !valid(this.root.owner)) return undefined;
    let entry = this.head;
    let visits = 0;
    for (const [resume, count] of this.#resumes) {
      if (count > visits) {
        entry = resume;
        visits = count;
      }
    }
    if (entry >> 12n !== this.head >> 12n) return undefined;
    let instructions = 0;
    let bytes = 0;
    const seeds: bigint[] = [];
    const ranked = [...this.#targets].sort((a, b) => b[1].visits - a[1].visits);
    const included = new Set<bigint>();
    for (const [pc, edge] of ranked) {
      if (seeds.length === 8) break;
      const { child } = edge;
      const alreadyIncluded = included.has(child.owner);
      if (
        edge.visits < 2 ||
        !valid(child.owner) ||
        (!alreadyIncluded &&
          (instructions + child.instructionCount > limits.maxInstructions ||
            bytes + child.sourceBytes > limits.maxSourceBytes))
      )
        continue;
      seeds.push(pc);
      if (!alreadyIncluded) {
        included.add(child.owner);
        instructions += child.instructionCount;
        bytes += child.sourceBytes;
      }
    }
    return seeds.length > 0 ? { entry, seeds } : undefined;
  }
}

interface Dispatch {
  from: bigint;
  to: bigint;
  artifact: Artifact;
}

interface LinearReturns {
  returns: number;
  resumes: Map<bigint, number>;
  targets: Map<bigint, Edge>;
}

export class LinearRegionTrace {
  readonly boundaryObserved = false;
  readonly #history: Dispatch[] = [];
  readonly #owners = new Map<Artifact, LinearReturns>();
  #samples = 0;

  sample(from: bigint, to: bigint, entry: InstalledEntry): boolean {
    const artifact = entry.artifact;
    let prior = this.#history.length - 1;
    while (prior >= 0 && this.#history[prior]?.artifact !== artifact) prior--;
    const previous = this.#history[prior];
    const recent = this.#history.at(-1);
    if (recent && recent.to !== from) {
      this.#history.length = 0;
    } else if (artifact.metadata.hasIndirectCall && previous && prior < this.#history.length - 1) {
      const page = from >> 12n;
      const path = this.#history.slice(prior + 1);
      if (
        previous.from >> 12n === page &&
        path.every(
          (step) =>
            step.from >> 12n === page &&
            step.artifact.metadata.contextVersion === artifact.metadata.contextVersion,
        )
      ) {
        let profile = this.#owners.get(artifact);
        if (!profile && this.#owners.size < 8) {
          profile = { returns: 0, resumes: new Map(), targets: new Map() };
          this.#owners.set(artifact, profile);
        }
        if (profile) {
          profile.returns++;
          if (profile.resumes.has(from) || profile.resumes.size < 16)
            profile.resumes.set(from, (profile.resumes.get(from) ?? 0) + 1);
          for (const step of path) {
            const child = step.artifact.metadata;
            const edge = profile.targets.get(step.from);
            if (edge?.child === child) {
              edge.visits++;
            } else if (!edge && profile.targets.size < 16) {
              profile.targets.set(step.from, { child, visits: 1 });
            }
          }
        }
      }
    }
    if (this.#history.length === 4) this.#history.shift();
    this.#history.push({ from, to, artifact });
    this.#samples++;
    if (this.#samples === 4096) return true;
    if (this.#samples !== 512) return false;
    // A stable single return needs less training than a changing target set.
    return [...this.#owners.values()].some(
      (profile) => profile.returns >= 128 && profile.targets.size === 1,
    );
  }

  checked(owners: WeakSet<Artifact>): void {
    for (const artifact of this.#owners.keys()) owners.add(artifact);
  }

  choose(
    limits: CompileLimits,
    valid: (owner: bigint) => boolean,
  ): { entry: bigint; seeds: bigint[] } | undefined {
    const ranked = [...this.#owners].sort((a, b) => b[1].returns - a[1].returns);
    for (const [artifact, profile] of ranked) {
      if (profile.returns < 32 || !valid(artifact.metadata.owner)) continue;
      const resume = [...profile.resumes].sort((a, b) => b[1] - a[1])[0];
      if (!resume || resume[1] * 4 < profile.returns * 3) continue;
      const entry = resume[0];
      let instructions = artifact.metadata.instructionCount;
      let bytes = artifact.metadata.sourceBytes;
      const seeds: bigint[] = [];
      const included = new Set<bigint>();
      for (const [pc, edge] of [...profile.targets].sort((a, b) => b[1].visits - a[1].visits)) {
        const { child } = edge;
        const admitted = included.has(child.owner);
        if (seeds.length === 8) break;
        if (
          pc === entry ||
          pc >> 12n !== entry >> 12n ||
          edge.visits < 4 ||
          !valid(child.owner) ||
          (!admitted &&
            (instructions + child.instructionCount > limits.maxInstructions ||
              bytes + child.sourceBytes > limits.maxSourceBytes))
        )
          continue;
        // Admit complete observed handlers to avoid turning their tails into
        // ownership boundaries as breadth-first decoding reaches its limit.
        seeds.push(pc);
        if (!admitted) {
          included.add(child.owner);
          instructions += child.instructionCount;
          bytes += child.sourceBytes;
        }
      }
      if (seeds.length > 0) return { entry, seeds };
    }
    return undefined;
  }
}
