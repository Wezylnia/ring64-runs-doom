import { byteLength, copyMemoryInput, unsigned64 } from './input.js';
import {
  backingName,
  captureBackings,
  captureMappings,
  validateBackings,
  validateMappings,
  type MemoryBackingInput,
  type MemoryMapping,
} from './memory.js';
import { SharedControl, SIGNAL, SLOT, STARTUP_WORD, nextIdentity } from './parallel/control.js';
import { handle, type ParallelCpu } from './parallel/handle.js';
import {
  captureMachine,
  findOffset,
  requirePlane,
  seedBacking,
  type BackingRecord,
  type MachineOptions,
} from './parallel/options.js';
import type {
  Command,
  FromWorker,
  LocalMethod,
  ParallelRunResult,
  Range,
  RunOptions,
  Start,
} from './parallel/protocol.js';
import { bindRam, ramModule } from './parallel/ram.js';
import {
  launchWorker,
  ParallelError,
  requireWorkerCapability,
  type Transport,
} from './parallel/transport.js';

export type { MachineOptions, ParallelCpu, ParallelRunResult, RunOptions };
export { ParallelError };

interface Deferred<T> {
  promise: Promise<T>;
  resolve(value: T): void;
  reject(cause: unknown): void;
}

function deferred<T>(): Deferred<T> {
  let resolve!: Deferred<T>['resolve'];
  let reject!: Deferred<T>['reject'];
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

type Pending = Deferred<unknown> & { sequence: bigint; method: Command['method'] };
type Ingress = { irq: number | null; nmiPending: boolean };

interface Peer {
  readonly id: number;
  readonly ready: Deferred<void>;
  transport?: Transport;
  command: Pending | undefined;
  admin: Pending | undefined;
  replacement: Promise<unknown> | undefined;
  waitingIrq: Promise<boolean> | undefined;
  waitingNmi: Promise<void> | undefined;
  sequence: bigint;
  portTicket: bigint;
  watches: Map<bigint, Range[]>;
}

type Job = {
  action(permit: bigint): Promise<unknown>;
  result: Deferred<unknown>;
};

interface Grant {
  peer: Peer;
  request: bigint;
  completed: Deferred<void>;
}

type Captured = ReturnType<typeof captureMachine>;

let initializeMachine: (
  machine: ParallelMachine,
  compiler: WebAssembly.Module,
  helper: WebAssembly.Module,
) => Promise<void>;

export interface Machine {
  readonly cpus: readonly ParallelCpu[];
  pause(): Promise<void>;
  reset(): Promise<void>;
  dispose(): Promise<void>;
  addBacking(input: MemoryBackingInput): Promise<void>;
  removeBacking(id: string): Promise<void>;
  setMappings(mappings: MemoryMapping[]): Promise<void>;
  readMemory(address: bigint, length: number): Promise<Uint8Array>;
  writeMemory(address: bigint, bytes: Uint8Array): Promise<void>;
  readPhysicalMemory(address: bigint, length: number): Promise<Uint8Array>;
  writePhysicalMemory(address: bigint, bytes: Uint8Array): Promise<void>;
}

export async function createMachine(options: MachineOptions): Promise<Machine> {
  const input = captureMachine(options);
  requireWorkerCapability();
  let helper: WebAssembly.Module;
  try {
    helper = ramModule();
  } catch (cause) {
    throw new ParallelError(
      'unsupported',
      'parallel execution requires Wasm multiple memories and atomics',
      { cause },
    );
  }
  const compiler =
    input.compiler instanceof WebAssembly.Module
      ? input.compiler
      : await WebAssembly.compile(input.compiler);
  if (
    !WebAssembly.Module.exports(compiler).some(
      ({ name, kind }) => name === 'core_parallel_abi' && kind === 'function',
    )
  )
    throw new ParallelError(
      'unsupported',
      'parallel execution requires the parallel Core artifact',
    );
  let memory: WebAssembly.Memory;
  try {
    memory = new WebAssembly.Memory({
      initial: input.layout.pages,
      maximum: input.layout.pages,
      shared: true,
    });
  } catch (cause) {
    throw new ParallelError('capacity', 'parallel RAM allocation failed', { cause });
  }
  const backingStore = bindRam(helper, new WebAssembly.Memory({ initial: 1 }), memory);
  const machine = new ParallelMachine(input, memory, backingStore.store_lane);
  await initializeMachine(machine, compiler, helper);
  return machine;
}

class ParallelMachine implements Machine {
  readonly #cpus: readonly ParallelCpu[];
  readonly #input: Captured;
  readonly #memory: WebAssembly.Memory;
  readonly #storeLane: (offset: number, value: bigint) => void;
  readonly #peers: Peer[];
  readonly #control: SharedControl;
  #state: 'starting' | 'idle' | 'disposing' | 'failed' | 'disposed' = 'starting';
  #failure: ParallelError | undefined;
  #dispose: Promise<void> | undefined;
  #termination: Promise<void> | undefined;
  #reservation: Promise<void> | undefined;
  #jobs: Job[] = [];
  #requests: Array<{ peer: Peer; request: bigint }> = [];
  #grant: Grant | undefined;
  #closed: Deferred<void> | undefined;
  #round: { phase: bigint; peers: Peer[]; finished: Deferred<void> } | undefined;
  #drain: Promise<void> | undefined;
  #writes: Range[] = [];
  #removeWatches: Range[][] = [];
  #watched = new Map<number, number>();
  #records = new Map<string, BackingRecord>();
  #maps: MemoryMapping[];
  #incarnation = 0n;
  #ingressSequence = 0n;
  #invalidation = 0n;
  #captureGeneration = 0n;

  static {
    initializeMachine = (machine, compiler, helper) => machine.#initialize(compiler, helper);
  }

  constructor(
    input: Captured,
    memory: WebAssembly.Memory,
    storeLane: (offset: number, value: bigint) => void,
  ) {
    this.#input = input;
    this.#memory = memory;
    this.#storeLane = storeLane;
    this.#maps = input.mappings;
    this.#control = new SharedControl(memory);
    this.#control.initialize();
    this.#peers = input.cpus.map((_, id) => ({
      id,
      ready: deferred(),
      command: undefined,
      admin: undefined,
      replacement: undefined,
      waitingIrq: undefined,
      waitingNmi: undefined,
      watches: new Map(),
      sequence: 0n,
      portTicket: 0n,
    }));
    for (const backing of input.backings) {
      const record = this.#record(backing);
      seedBacking(this.#storeLane, record.offset, backing.bytes);
      this.#records.set(backing.id, record);
    }
    input.backings.length = 0;
    this.#cpus = Object.freeze(
      this.#peers.map((peer) =>
        handle(
          {
            id: peer.id,
            run: (options) => this.#run(peer, options),
            pause: () => {
              this.#check();
              return this.#pausePeer(peer);
            },
            requestInterrupt: (vector) => this.#interrupt(peer, vector),
            requestNmi: () => this.#nmi(peer),
          },
          (method, args) => this.#local(peer, method, args),
        ),
      ),
    );
  }

  get cpus(): readonly ParallelCpu[] {
    return this.#cpus;
  }

  async #initialize(compiler: WebAssembly.Module, helper: WebAssembly.Module): Promise<void> {
    for (const { ready } of this.#peers) void ready.promise.catch(() => {});
    let timer: ReturnType<typeof setTimeout> | undefined;
    const starts = this.#peers.map(async (peer) => {
      const transport = await launchWorker(
        (message) => this.#receive(peer, message),
        (cause) => this.#fail(cause),
      );
      peer.transport = transport;
      if (this.#failure) {
        await transport.terminate();
        throw this.#failure;
      }
      const start: Start = {
        kind: 'start',
        id: peer.id,
        compiler,
        ramModule: helper,
        shared: this.#memory,
        layout: this.#input.layout,
        model: this.#input.memoryModel,
        memory: { memoryLimits: this.#input.memoryLimits, mappings: this.#maps },
        limits: this.#input.limits,
        state: this.#input.cpus[peer.id]!,
        backings: [...this.#records.values()],
        hasIo: this.#input.io !== undefined,
      };
      try {
        transport.post(start);
      } catch (cause) {
        if (cause instanceof DOMException && cause.name === 'DataCloneError')
          throw new ParallelError(
            'unsupported',
            'parallel workers cannot clone shared Wasm memory or modules',
            { cause },
          );
        throw cause;
      }
      await peer.ready.promise;
    });
    try {
      await Promise.race([
        Promise.all(starts),
        new Promise<never>((_, reject) => {
          timer = setTimeout(
            () => reject(new ParallelError('failed', 'parallel worker startup deadline exceeded')),
            30000,
          );
        }),
      ]);
      if (
        Atomics.load(new BigUint64Array(this.#memory.buffer), STARTUP_WORD / 8) !==
        BigInt(this.#peers.length)
      )
        throw new Error('parallel worker atomic handshake failed');
      if (this.#failure) throw this.#failure;
      this.#state = 'idle';
    } catch (cause) {
      this.#fail(cause);
      await Promise.allSettled(starts);
      await this.#terminate();
      throw this.#failure;
    } finally {
      clearTimeout(timer);
    }
  }

  #check(reserved = false): void {
    if (this.#inProvider) throw new ParallelError('busy', 'reentrant parallel port callback');
    if (this.#failure) throw this.#failure;
    if (this.#state === 'disposed' || this.#state === 'disposing')
      throw new ParallelError('disposed', 'parallel machine is disposed');
    if (!reserved && this.#reservation)
      throw new ParallelError('busy', 'parallel machine admissions are reserved');
  }

  #capture<T>(read: () => T): T {
    const generation = this.#captureGeneration;
    const captured = read();
    this.#check();
    if (generation !== this.#captureGeneration)
      throw new ParallelError('busy', 'parallel command started during input capture');
    return captured;
  }

  #record(backing: MemoryBackingInput): BackingRecord {
    const offset = findOffset(this.#input.layout, this.#records.values(), backing.bytes.length);
    this.#incarnation = nextIdentity(this.#incarnation);
    return { id: backing.id, length: backing.bytes.length, offset, incarnation: this.#incarnation };
  }

  #lengths(): Map<string, number> {
    return new Map([...this.#records].map(([id, record]) => [id, record.length]));
  }

  #terminate(): Promise<void> {
    return (this.#termination ??= Promise.allSettled(
      this.#peers.map((peer) => peer.transport?.terminate()),
    ).then(() => {}));
  }

  #fail(cause: unknown): void {
    if (this.#failure || this.#state === 'disposed') return;
    this.#failure =
      cause instanceof ParallelError
        ? cause
        : new ParallelError('failed', 'parallel machine failed', { cause });
    this.#state = 'failed';
    for (const peer of this.#peers) {
      peer.ready.reject(this.#failure);
      peer.command?.reject(this.#failure);
      peer.admin?.reject(this.#failure);
    }
    for (const job of this.#jobs.splice(0)) job.result.reject(this.#failure);
    this.#grant?.completed.reject(this.#failure);
    this.#closed?.reject(this.#failure);
    this.#round?.finished.reject(this.#failure);
    void this.#terminate();
  }

  #receive(peer: Peer, input: unknown): void {
    if (this.#failure || this.#state === 'disposed') return;
    const message = input as FromWorker;
    try {
      switch (message.kind) {
        case 'ready':
          if (this.#state !== 'starting') throw new Error('unexpected parallel worker readiness');
          peer.ready.resolve();
          return;
        case 'fatal':
          throw new Error(message.message);
        case 'ack':
          this.#acknowledged();
          return;
        case 'round':
          this.#roundComplete();
          return;
        case 'result':
        case 'rejected': {
          const admin = peer.admin?.sequence === message.sequence;
          const pending = admin ? peer.admin : peer.command;
          if (!pending || pending.sequence !== message.sequence)
            throw new Error('stale parallel command response');
          if (admin) peer.admin = undefined;
          else peer.command = undefined;
          if (pending.method === 'run') {
            this.#control.store(peer.id, SLOT.run, 0n);
            this.#control.store(peer.id, SLOT.pause, 0n);
            this.#control.clearSignal(peer.id, SIGNAL.pause);
          }
          if (message.kind === 'result') pending.resolve(message.result);
          else
            pending.reject(
              new (message.error === 'TypeError' ? TypeError : RangeError)(message.message),
            );
          return;
        }
        case 'exclusive':
          if (this.#requests.some((request) => request.peer === peer) || this.#grant?.peer === peer)
            throw new Error('duplicate worker continuation request');
          this.#requests.push({ peer, request: message.request });
          this.#schedule();
          return;
        case 'released':
          if (this.#grant?.peer !== peer || this.#grant.request !== message.request)
            throw new Error('unmatched parallel grant release');
          this.#grant.completed.resolve();
          return;
        case 'write':
          if (this.#grant?.peer !== peer && !peer.admin)
            throw new Error('parallel write publication lacked a grant');
          this.#writes.push(message.range);
          return;
        case 'watch':
          if (this.#grant?.peer !== peer)
            throw new Error('parallel watch publication lacked a grant');
          this.#rememberWatch(peer, message.owner, message.ranges);
          return;
        case 'unwatch': {
          const old = peer.watches.get(message.owner);
          if (old) {
            peer.watches.delete(message.owner);
            this.#removeWatches.push(old);
            this.#schedule();
          }
          return;
        }
        case 'io':
          this.#port(peer, message);
          return;
        default:
          throw new Error('unknown parallel worker delivery');
      }
    } catch (cause) {
      this.#fail(cause);
    }
  }

  #inProvider = false;

  #port(peer: Peer, message: Extract<FromWorker, { kind: 'io' }>): void {
    const binding = this.#input.io;
    if (
      !binding ||
      this.#inProvider ||
      peer.command?.method !== 'run' ||
      peer.command.sequence !== message.sequence ||
      message.ticket !== nextIdentity(peer.portTicket) ||
      !Number.isInteger(message.port) ||
      message.port < 0 ||
      message.port > 65535 ||
      ![8, 16, 32].includes(message.width)
    )
      throw new Error('invalid parallel port request');
    peer.portTicket = message.ticket;
    this.#inProvider = true;
    try {
      let value = 0;
      if (message.value === undefined) {
        value = Reflect.apply(binding.read, binding.receiver, [
          message.port,
          message.width,
        ]) as number;
        if (
          typeof value !== 'number' ||
          !Number.isInteger(value) ||
          value < 0 ||
          value >= 2 ** message.width
        )
          throw new TypeError('io.read must return an unsigned integer fitting its width');
      } else if (
        Reflect.apply(binding.write, binding.receiver, [
          message.port,
          message.width,
          message.value,
        ]) !== undefined
      ) {
        throw new TypeError('io.write must return undefined');
      }
      this.#control.storeWord(peer.id, SLOT.ioResult, value);
      this.#control.storeWord(peer.id, SLOT.ioStatus, 1);
    } catch (cause) {
      this.#control.storeWord(peer.id, SLOT.ioStatus, -1);
      throw new Error('parallel port provider failed', { cause });
    } finally {
      this.#inProvider = false;
      this.#control.wake(peer.id);
    }
  }

  #rememberWatch(peer: Peer, owner: bigint, ranges: Range[]): void {
    if (peer.watches.has(owner)) throw new Error('duplicate parallel source watch');
    peer.watches.set(owner, ranges);
    for (const { offset, length } of ranges)
      for (let i = offset; i < offset + length; i++)
        this.#watched.set(i, (this.#watched.get(i) ?? 0) + 1);
  }

  #sweepWatches(): void {
    const words = new Int32Array(this.#memory.buffer);
    for (const ranges of this.#removeWatches.splice(0)) {
      for (const { offset, length } of ranges) {
        for (let i = offset; i < offset + length; i++) {
          const count = this.#watched.get(i)! - 1;
          if (count === 0) {
            this.#watched.delete(i);
            const byte = i - this.#input.layout.ramBase;
            Atomics.and(
              words,
              this.#input.layout.watchBase / 4 + Math.floor(byte / 32),
              ~(1 << (byte % 32)),
            );
          } else {
            this.#watched.set(i, count);
          }
        }
      }
    }
  }

  #acknowledged(): void {
    const phase = this.#control.phase;
    if (
      this.#closed &&
      this.#peers.every(({ id }) => this.#control.load(id, SLOT.acknowledgement) === phase)
    )
      this.#closed.resolve();
  }

  #roundComplete(): void {
    const round = this.#round;
    if (
      round &&
      round.peers.every(
        ({ id }) =>
          this.#control.loadWord(id, SLOT.ready) === 0 ||
          this.#control.load(id, SLOT.roundAck) === round.phase,
      )
    )
      round.finished.resolve();
  }

  async #reopen(): Promise<void> {
    if (this.#jobs.length || this.#requests.length || this.#removeWatches.length) {
      const phase = nextIdentity(this.#control.phase);
      const peers = this.#peers.filter(
        (peer) =>
          peer.command?.method === 'run' &&
          this.#control.loadWord(peer.id, SLOT.ready) === 1 &&
          !this.#requests.some((request) => request.peer === peer),
      );
      if (peers.length > 0) {
        this.#round = { phase, peers, finished: deferred<void>() };
        for (const { id } of peers) this.#control.store(id, SLOT.round, phase);
      }
    }
    this.#control.transition(false);
    this.#notice(false);
    if (!this.#round) return;
    this.#roundComplete();
    await this.#round.finished.promise;
    for (const { id } of this.#round.peers) this.#control.store(id, SLOT.round, 0n);
    this.#round = undefined;
  }

  #notice(closing: boolean): void {
    for (const { id, transport } of this.#peers) {
      if (closing) this.#control.signal(id, SIGNAL.phase);
      else {
        this.#control.clearSignal(id, SIGNAL.phase);
        this.#control.wake(id);
      }
      transport!.post({ kind: 'phase' });
    }
  }

  #schedule(): void {
    if (this.#drain || this.#failure) return;
    this.#drain = this.#barriers()
      .catch((cause: unknown) => this.#fail(cause))
      .finally(() => {
        this.#drain = undefined;
        if (
          !this.#failure &&
          (this.#jobs.length || this.#requests.length || this.#removeWatches.length)
        )
          this.#schedule();
      });
  }

  async #barriers(): Promise<void> {
    while (
      !this.#failure &&
      (this.#jobs.length || this.#requests.length || this.#removeWatches.length)
    ) {
      const phase = this.#control.transition(true);
      const closed = deferred<void>();
      this.#closed = closed;
      this.#notice(true);
      this.#acknowledged();
      await closed.promise;
      this.#closed = undefined;
      // A stopped requester may have an already reached access whose physical addresses are pinned.
      // Finish all such accesses before a coordinator mutation can replace those mappings.
      while (this.#requests.length > 0) {
        const grant = { ...this.#requests.shift()!, completed: deferred<void>() };
        this.#grant = grant;
        this.#control.store(grant.peer.id, SLOT.grant, grant.request);
        this.#control.wake(grant.peer.id);
        await grant.completed.promise;
        this.#control.store(grant.peer.id, SLOT.grant, 0n);
        this.#grant = undefined;
        if (this.#jobs.length === 0) break;
      }
      const job = this.#jobs.shift();
      if (job) {
        try {
          job.result.resolve(await job.action(phase));
        } catch (cause) {
          job.result.reject(cause);
          if (!(
            cause instanceof TypeError ||
            cause instanceof RangeError ||
            cause instanceof ParallelError
          ))
            throw cause;
        }
      }
      if (this.#failure) return;
      this.#sweepWatches();
      const ranges = this.#writes.splice(0);
      if (ranges.length > 0) {
        const sequence = (this.#invalidation = nextIdentity(this.#invalidation));
        for (const { id, transport } of this.#peers) {
          this.#control.store(id, SLOT.invalidations, sequence);
          transport!.post({ kind: 'invalidate', sequence, ranges });
        }
      }
      await this.#reopen();
    }
  }

  #admin<T>(action: (permit: bigint) => Promise<T>): Promise<T> {
    if (this.#jobs.length >= this.#peers.length + 1)
      throw new ParallelError('capacity', 'parallel administration queue is full');
    this.#captureGeneration = nextIdentity(this.#captureGeneration);
    const result = deferred<unknown>();
    this.#jobs.push({ action, result });
    this.#schedule();
    return result.promise as Promise<T>;
  }

  #start(
    peer: Peer,
    method: Command['method'],
    args: unknown[],
    permit?: bigint,
  ): Promise<unknown> {
    if (permit === undefined ? peer.command !== undefined : peer.admin !== undefined)
      throw new ParallelError('busy', 'parallel CPU has an outstanding command');
    if (this.#failure) return Promise.reject(this.#failure);
    this.#captureGeneration = nextIdentity(this.#captureGeneration);
    peer.sequence = nextIdentity(peer.sequence);
    const sequence = peer.sequence;
    const pending: Pending = { ...deferred(), sequence, method };
    if (permit === undefined) peer.command = pending;
    else peer.admin = pending;
    if (method === 'run') this.#control.store(peer.id, SLOT.run, sequence);
    try {
      peer.transport!.post({
        kind: 'command',
        sequence,
        method,
        args,
        ...(permit === undefined ? {} : { permit }),
      });
    } catch (cause) {
      this.#fail(cause);
    }
    return pending.promise;
  }

  #run(peer: Peer, options: RunOptions): Promise<ParallelRunResult> {
    this.#check();
    const input = this.#capture(() => structuredClone(options));
    return this.#start(peer, 'run', [input]) as Promise<ParallelRunResult>;
  }

  #local(peer: Peer, method: LocalMethod, args: unknown[]): Promise<unknown> {
    this.#check();
    if (method === 'reset' && this.#input.memoryModel !== 'physical')
      throw new TypeError('physical reset required');
    const input = this.#capture(() => structuredClone(args));
    if (method === 'setState' || method === 'reset')
      return this.#replace(peer, this.#start(peer, method, input));
    return this.#start(peer, method, input);
  }

  #pausePeer(peer: Peer): Promise<void> {
    const command = peer.command;
    if (!command || command.method !== 'run') return Promise.resolve();
    this.#control.store(peer.id, SLOT.pause, command.sequence);
    this.#control.signal(peer.id, SIGNAL.pause);
    peer.transport!.post({ kind: 'phase' });
    return command.promise.then(() => {});
  }

  #reserve(reset: boolean): Promise<void> {
    this.#check();
    this.#captureGeneration = nextIdentity(this.#captureGeneration);
    const result = deferred<void>();
    this.#reservation = result.promise;
    void (async () => {
      try {
        await Promise.all(
          this.#peers.map((peer) =>
            peer.command?.method === 'run' ? this.#pausePeer(peer) : peer.command?.promise,
          ),
        );
        if (reset)
          await this.#admin(async (permit) => {
            await Promise.all(
              this.#peers.map((peer) =>
                this.#replace(peer, this.#start(peer, 'reset', [], permit)),
              ),
            );
          });
        result.resolve();
      } catch (cause) {
        result.reject(cause);
      } finally {
        this.#reservation = undefined;
      }
    })();
    return result.promise;
  }

  pause(): Promise<void> {
    return this.#reserve(false);
  }

  reset(): Promise<void> {
    this.#check();
    if (this.#input.memoryModel !== 'physical') throw new TypeError('physical reset required');
    return this.#reserve(true);
  }

  dispose(): Promise<void> {
    if (this.#inProvider) throw new ParallelError('busy', 'reentrant parallel port callback');
    if (this.#dispose) return this.#dispose;
    if (this.#state !== 'failed') this.#state = 'disposing';
    this.#dispose = (async () => {
      try {
        await Promise.allSettled([
          this.#reservation,
          ...this.#peers.map((peer) =>
            peer.command?.method === 'run' ? this.#pausePeer(peer) : peer.command?.promise,
          ),
        ]);
        await this.#drain;
      } finally {
        await this.#terminate();
        this.#state = 'disposed';
      }
    })();
    return this.#dispose;
  }

  #ingressId(): bigint {
    this.#ingressSequence = nextIdentity(this.#ingressSequence);
    if (this.#ingressSequence >= 1n << 55n) {
      const failure = new ParallelError('capacity', 'parallel ingress sequence exhausted');
      this.#fail(failure);
      throw failure;
    }
    return this.#ingressSequence;
  }

  #replace(peer: Peer, pending: Promise<unknown>): Promise<void> {
    const replaced = pending
      .then((result) => {
        const { irq, nmiPending } = result as Ingress;
        this.#control.store(
          peer.id,
          SLOT.irq,
          irq === null ? 0n : (this.#ingressId() << 9n) | BigInt(irq + 1),
        );
        this.#control.store(peer.id, SLOT.nmi, nmiPending ? this.#ingressId() : 0n);
      })
      .finally(() => {
        peer.replacement = undefined;
      });
    peer.replacement = replaced;
    return replaced;
  }

  #interrupt(peer: Peer, vector: number): Promise<boolean> {
    this.#check(true);
    if (typeof vector !== 'number' || !Number.isInteger(vector) || vector < 0 || vector > 255)
      throw new TypeError('IRQ vector must be an unsigned 8-bit integer number');
    if (peer.replacement) {
      if (peer.waitingIrq) return Promise.resolve(false);
      const waiting = peer.replacement
        .then(
          () => this.#interrupt(peer, vector),
          () => this.#interrupt(peer, vector),
        )
        .finally(() => {
          peer.waitingIrq = undefined;
        });
      peer.waitingIrq = waiting;
      return waiting;
    }
    if (this.#control.load(peer.id, SLOT.irq) !== 0n) return Promise.resolve(false);
    const identity = (this.#ingressId() << 9n) | BigInt(vector + 1);
    if (this.#control.compareExchange(peer.id, SLOT.irq, 0n, identity) !== 0n)
      return Promise.resolve(false);
    this.#control.signal(peer.id, SIGNAL.ingress);
    return Promise.resolve(true);
  }

  #nmi(peer: Peer): Promise<void> {
    this.#check(true);
    if (peer.replacement) {
      if (peer.waitingNmi) return peer.waitingNmi;
      const waiting = peer.replacement
        .then(
          () => this.#nmi(peer),
          () => this.#nmi(peer),
        )
        .finally(() => {
          peer.waitingNmi = undefined;
        });
      peer.waitingNmi = waiting;
      return waiting;
    }
    if (this.#control.load(peer.id, SLOT.nmi) === 0n) {
      this.#control.compareExchange(peer.id, SLOT.nmi, 0n, this.#ingressId());
      this.#control.signal(peer.id, SIGNAL.ingress);
    }
    return Promise.resolve();
  }

  async #all(method: Command['method'], args: unknown[], permit: bigint): Promise<void> {
    try {
      await Promise.all(this.#peers.map((peer) => this.#start(peer, method, args, permit)));
    } catch (cause) {
      this.#fail(cause);
      throw this.#failure;
    }
  }

  addBacking(input: MemoryBackingInput): Promise<void> {
    this.#check();
    const [backing] = this.#capture(() =>
      captureBackings([input], this.#input.memoryLimits, this.#lengths()),
    );
    return this.#admin(async (permit) => {
      validateBackings([backing!], this.#lengths(), this.#input.memoryLimits);
      const record = this.#record(backing!);
      seedBacking(this.#storeLane, record.offset, backing!.bytes);
      await this.#all('addBacking', [record], permit);
      this.#records.set(record.id, record);
    });
  }

  removeBacking(id: string): Promise<void> {
    this.#check();
    backingName(id);
    return this.#admin(async (permit) => {
      const record = this.#records.get(id);
      if (!record) throw new TypeError('unknown backing name');
      if (this.#maps.some((mapping) => mapping.backing === id))
        throw new TypeError('cannot remove a mapped backing');
      await this.#all('removeBacking', [id], permit);
      this.#records.delete(id);
      const { ramBase, guardBase } = this.#input.layout;
      const words = new Int32Array(this.#memory.buffer);
      const first = guardBase / 4 + (record.offset - ramBase) / 64;
      const slots = Math.ceil(record.length / 4096) * 64;
      for (let i = 0; i < slots; i++) Atomics.store(words, first + i, 0);
    });
  }

  setMappings(mappings: MemoryMapping[]): Promise<void> {
    this.#check();
    const input = this.#capture(() =>
      captureMappings(mappings, this.#input.memoryLimits, this.#input.memoryModel),
    );
    return this.#admin(async (permit) => {
      validateMappings(input, this.#lengths());
      await this.#all('setMappings', [input], permit);
      this.#maps = input;
    });
  }

  #read(address: bigint, length: number, physical: boolean): Promise<Uint8Array> {
    this.#check();
    requirePlane(this.#input.memoryModel, physical);
    unsigned64(address, 'address');
    byteLength(length, 'length');
    return this.#admin(
      (permit) =>
        this.#start(
          this.#peers[0]!,
          physical ? 'readPhysicalMemory' : 'readMemory',
          [address, length],
          permit,
        ) as Promise<Uint8Array>,
    );
  }

  #write(address: bigint, bytes: Uint8Array, physical: boolean): Promise<void> {
    this.#check();
    requirePlane(this.#input.memoryModel, physical);
    unsigned64(address, 'address');
    const input = this.#capture(() => copyMemoryInput(bytes));
    return this.#admin(async (permit) => {
      await this.#start(
        this.#peers[0]!,
        physical ? 'writePhysicalMemory' : 'writeMemory',
        [address, input],
        permit,
      );
      await this.#all('hostMemoryChanged', [], permit);
    });
  }

  readMemory(address: bigint, length: number): Promise<Uint8Array> {
    return this.#read(address, length, false);
  }
  readPhysicalMemory(address: bigint, length: number): Promise<Uint8Array> {
    return this.#read(address, length, true);
  }
  writeMemory(address: bigint, bytes: Uint8Array): Promise<void> {
    return this.#write(address, bytes, false);
  }
  writePhysicalMemory(address: bigint, bytes: Uint8Array): Promise<void> {
    return this.#write(address, bytes, true);
  }
}
