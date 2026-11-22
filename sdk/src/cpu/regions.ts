import type { CompileLimits, UnitMetadata } from '../wasm/core.js';
import type { InstalledEntry } from './translations.js';

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
