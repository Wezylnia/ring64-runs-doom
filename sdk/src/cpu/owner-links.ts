import type { GeneratedRun } from '../wasm/abi.js';
import type { Core } from '../wasm/core.js';
import { linkedRunBytes, maxLinkedTargets, type LinkedRun } from '../wasm/linked-run.js';
import type { Artifact, InstalledEntry, Translations } from './translations.js';

const maxProfileEntries = 32;
const minTargetUses = 32;

export class OwnerLinks {
  readonly #runs = new Map<Artifact, GeneratedRun>();

  constructor(
    private readonly artifacts: readonly Artifact[],
    private readonly linked: LinkedRun,
  ) {
    artifacts.forEach((artifact, slot) => {
      this.#runs.set(artifact, linked.runs[slot]!);
    });
  }

  run(artifact: Artifact): GeneratedRun | undefined {
    return this.#runs.get(artifact);
  }

  receipt(cache: Translations): { continuations: number; invalidated: boolean } {
    const continuations = Number(this.linked.count.value) - 1;
    if (continuations > 0) {
      let lastUse = this.linked.lastUse.value as bigint;
      const used: { artifact: Artifact; ordinal: number }[] = [];
      for (const artifact of this.artifacts) {
        const ordinal = Number(lastUse & 255n);
        lastUse >>= 8n;
        if (ordinal) used.push({ artifact, ordinal });
      }
      used.sort((a, b) => a.ordinal - b.ordinal);
      for (const { artifact } of used) cache.touch(artifact);
    }
    return { continuations, invalidated: this.linked.invalidated.value !== 0 };
  }
}

export class OwnerLinkProfile {
  readonly #entries = new Map<bigint, { entry: InstalledEntry; uses: number }>();

  record(pc: bigint, entry: InstalledEntry): void {
    const existing = this.#entries.get(pc);
    if (existing?.entry === entry) {
      existing.uses++;
    } else if (existing || this.#entries.size < maxProfileEntries) {
      this.#entries.set(pc, { entry, uses: 1 });
    }
  }

  install(
    core: Core,
    cache: Translations,
    byteLimit: number,
    timings: { browserCompileMs: number; instantiationMs: number },
  ): number {
    const selected = [...this.#entries]
      .filter(
        ([pc, { entry, uses }]) =>
          uses >= minTargetUses && cache.candidates(pc, false, true)[0] === entry,
      )
      .sort((a, b) => b[1].uses - a[1].uses)
      .slice(0, maxLinkedTargets);
    const artifacts = [...new Set(selected.map(([, { entry }]) => entry.artifact))];
    if (artifacts.length < 2) return 0;
    const runs = artifacts.map(({ run }) => run);
    const targets = selected.map(
      ([
        pc,
        {
          entry: { artifact, index },
        },
      ]) => ({
        pc,
        owner: artifact.metadata.owner,
        entry: index,
        slot: artifacts.indexOf(artifact),
      }),
    );
    const bytes = linkedRunBytes(runs.length, targets);
    if (bytes.length > byteLimit) return 0;
    let began = performance.now();
    const module = new WebAssembly.Module(bytes);
    timings.browserCompileMs += performance.now() - began;
    began = performance.now();
    const linked = core.instantiateLinkedRun(module, runs);
    timings.instantiationMs += performance.now() - began;
    cache.installLinks(new OwnerLinks(artifacts, linked), bytes.length);
    return bytes.length;
  }
}
